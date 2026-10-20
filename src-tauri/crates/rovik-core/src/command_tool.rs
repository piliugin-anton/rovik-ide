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

use crate::overlay::chats::ChatOverlay;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 600_000;
/// Kept per stream: the start (what ran) and the end, where errors and
/// summaries usually are. What's in between is dropped and counted.
const HEAD_BYTES: usize = 16 * 1024;
const TAIL_BYTES: usize = 48 * 1024;
/// A check's whole output, kept too for the verify gate's failure keys
/// (V07): every error counts when rounds are compared, the middle ones
/// too. Past this a stream keeps its start and its last WHOLE_TAIL_BYTES.
const WHOLE_BYTES: usize = 8 * 1024 * 1024;
const WHOLE_TAIL_BYTES: usize = 1024 * 1024;

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
    /// `Queued::whole_output`: stdout and stderr up to WHOLE_BYTES each,
    /// for what `stdout` and `stderr` leave out (V07).
    #[serde(skip)]
    pub whole: Option<(String, String)>,
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
    /// An isolated chat's command: the project's path shows its view, and
    /// the credential stores are hidden. None: the command still gets the
    /// hiding-only policy, where this machine can enforce one.
    pub confine: Option<crate::overlay::confine::Confine>,
    /// Keep the whole output too (`CommandOutput::whole`): the verify
    /// gate's checks (V07).
    pub whole_output: bool,
}

impl Queued {
    /// A chat's command as `kind` queues it, run in its overlay's
    /// environment when the chat is isolated (`overlay`), with nothing to
    /// note while it waits and the output kept as usual.
    pub fn of(kind: crate::workload::JobKind, overlay: Option<&ChatOverlay>) -> Queued {
        Queued {
            kind,
            note: Box::new(|_| {}),
            env: overlay.map(|o| o.command_env()).unwrap_or_default(),
            confine: overlay.and_then(|o| o.confine()),
            whole_output: false,
        }
    }

    /// A chat's check (a test, a lint, an install), as `owner`'s.
    pub fn check(owner: &str, overlay: Option<&ChatOverlay>) -> Queued {
        let owner = owner.to_string();
        Queued::of(crate::workload::JobKind::Check { owner }, overlay)
    }

    /// With `note` shown what the command waits for.
    pub fn noting(mut self, note: impl Fn(Option<String>) + Send + Sync + 'static) -> Queued {
        self.note = Box::new(note);
        self
    }

    /// With the whole output kept too, when `whole_output`.
    pub fn whole(mut self, whole_output: bool) -> Queued {
        self.whole_output = whole_output;
        self
    }
}

/// No Windows mechanism yet (overlay/confine.rs, `windows`): the policy is
/// ignored here and the callers' text checks apply. When one is added, build
/// the line below through `confine::command`, as the Unix one does, keeping
/// the `raw_arg` form cmd.exe needs.
#[cfg(windows)]
fn shell_command(command: &str, _confine: Option<&crate::overlay::confine::Confine>) -> Command {
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
/// it's fish, nu or another shell with a syntax of its own. With a
/// `confine` policy it runs confined: the project's path shows the chat's
/// view, and the credential stores are hidden (overlay/confine.rs).
#[cfg(not(windows))]
fn shell_command(command: &str, confine: Option<&crate::overlay::confine::Confine>) -> Command {
    use crate::process::{shell_kind, user_shell, ShellKind};
    let shell = user_shell()
        .filter(|s| shell_kind(s) == ShellKind::Posix)
        .unwrap_or_else(|| "/bin/sh".to_string());
    let mut cmd = match confine {
        Some(c) => crate::overlay::confine::command(c, &shell, &["-c", command]),
        None => {
            let mut cmd = crate::process::command(shell);
            cmd.args(["-c", command]);
            cmd
        }
    };
    crate::process::ordinary_oom_score(&mut cmd);
    cmd
}

/// Keeps the first `HEAD_BYTES` and the last `TAIL_BYTES` of a stream. Only
/// keeping the start lost what matters most in long output: a build's or a
/// test run's errors and final summary come at the end.
#[derive(Clone)]
struct HeadTail {
    head: Vec<u8>,
    tail: Vec<u8>,
    /// Bytes dropped from the front of `tail` so far.
    dropped: usize,
    head_bytes: usize,
    tail_bytes: usize,
}

impl HeadTail {
    fn new() -> Self {
        Self::sized(HEAD_BYTES, TAIL_BYTES)
    }

    fn sized(head_bytes: usize, tail_bytes: usize) -> Self {
        HeadTail {
            head: Vec::new(),
            tail: Vec::new(),
            dropped: 0,
            head_bytes,
            tail_bytes,
        }
    }

    fn push(&mut self, mut bytes: &[u8]) {
        if self.head.len() < self.head_bytes {
            let take = bytes.len().min(self.head_bytes - self.head.len());
            self.head.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
        }
        self.tail.extend_from_slice(bytes);
        // Trim in batches, not on every read.
        if self.tail.len() > 2 * self.tail_bytes {
            let cut = self.tail.len() - self.tail_bytes;
            self.tail.drain(..cut);
            self.dropped += cut;
        }
    }

    /// The kept output, and whether anything was dropped.
    fn finish(mut self) -> (Vec<u8>, bool) {
        if self.tail.len() > self.tail_bytes {
            let cut = self.tail.len() - self.tail_bytes;
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

/// A stream as kept: its start and end, whether anything was dropped, and
/// with `whole`, up to WHOLE_BYTES of it (V07).
type Drained = ((Vec<u8>, bool), Option<Vec<u8>>);

/// What a stream's reader has read so far: its start and end, and the
/// whole of it when asked for.
#[derive(Clone)]
struct Draining {
    kept: HeadTail,
    all: Option<HeadTail>,
}

impl Draining {
    fn finish(self, cut: bool) -> Drained {
        let (bytes, dropped) = self.kept.finish();
        ((bytes, dropped || cut), self.all.map(|a| a.finish().0))
    }
}

/// Reads a whole stream on its own thread, keeping its start and end.
/// What it has read is there at any time: a timeout that can't wait for
/// the stream's end (a process that left the group holds it) takes that.
fn drain<R: Read + Send + 'static>(
    mut reader: R,
    whole: bool,
) -> (thread::JoinHandle<()>, Arc<Mutex<Draining>>) {
    let draining = Arc::new(Mutex::new(Draining {
        kept: HeadTail::new(),
        all: whole.then(|| HeadTail::sized(WHOLE_BYTES - WHOLE_TAIL_BYTES, WHOLE_TAIL_BYTES)),
    }));
    let shared = draining.clone();
    let reading = thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut read = shared.lock().unwrap_or_else(|e| e.into_inner());
                    read.kept.push(&buf[..n]);
                    if let Some(all) = read.all.as_mut() {
                        all.push(&buf[..n]);
                    }
                }
            }
        }
    });
    (reading, draining)
}

/// A started shell (or tool) and everything it starts, which a timeout or
/// a Stop kills together: `Child::kill` alone only ends the shell, leaving
/// e.g. a dev server it ran alive and holding the output pipes.
///
/// On Unix the shell leads its own process group (the caller spawned it
/// with `process_group(0)`), which outlives it while any member is alive.
/// On Windows it goes into a job object as it starts: what its members
/// start is in the job too, even once their parent has exited, and the job
/// ends them all at once. `taskkill /T` ended them one at a time, so a
/// script whose child it ended first went on to its next command
/// meanwhile (`cmd /c "slow & next"`), and it found nothing of a tree
/// whose first process had exited. Where no job can be made, taskkill's
/// tree it is.
pub struct ProcessTree {
    #[cfg(windows)]
    job: Option<job_object::JobObject>,
}

impl ProcessTree {
    /// The tree of `child`, just spawned: taken at once, before it has
    /// had the time to start anything outside it.
    #[cfg_attr(not(windows), allow(unused_variables))]
    pub fn of(child: &std::process::Child) -> ProcessTree {
        ProcessTree {
            #[cfg(windows)]
            job: job_object::JobObject::of(child),
        }
    }

    /// Kills the shell and everything it started.
    pub fn kill(&self, child: &mut std::process::Child) {
        // The group outlives the shell while any member is alive, so this
        // works after it has exited too.
        #[cfg(unix)]
        if let Ok(pid) = libc::pid_t::try_from(child.id()) {
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
        #[cfg(windows)]
        if !self.job.as_ref().is_some_and(job_object::JobObject::kill) {
            let _ = crate::process::no_window(&mut Command::new("taskkill"))
                .args(["/T", "/F", "/PID", &child.id().to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = child.kill();
    }
}

/// A job object holding a started process, and so all it starts.
#[cfg(windows)]
mod job_object {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
    };

    pub struct JobObject(HANDLE);

    // A job's handle may be used, and closed, from any thread.
    unsafe impl Send for JobObject {}
    unsafe impl Sync for JobObject {}

    impl JobObject {
        /// A new job holding `child`; None where it can't be made or
        /// `child` can't go in it (before Windows 8, with the app in a job
        /// of its own already).
        pub fn of(child: &std::process::Child) -> Option<JobObject> {
            use std::os::windows::io::AsRawHandle;
            // SAFETY: a new unnamed job, whose handle Drop closes once;
            // the child's handle is open while `child` is.
            unsafe {
                let job = JobObject(CreateJobObjectW(None, PCWSTR::null()).ok()?);
                AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle())).ok()?;
                Some(job)
            }
        }

        /// Ends every process in the job: whether it could.
        pub fn kill(&self) -> bool {
            // SAFETY: the handle is open until Drop.
            unsafe { TerminateJobObject(self.0, 1).is_ok() }
        }
    }

    impl Drop for JobObject {
        fn drop(&mut self) {
            // Closing it ends nothing: a command's background process that
            // outlives a normal end goes on, as on Unix.
            // SAFETY: opened in `of`, closed only here.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// Whether the shell has exited, without reaping it. A reaped shell's pid
/// can go to a new process once its group is empty (say, the only process
/// still holding the pipes left the group with setsid), and a kill of its
/// tree would then signal that stranger's group. The unreaped zombie keeps
/// the pid, and so the group id, ours until `wait`.
#[cfg(target_os = "linux")]
pub fn shell_exited(child: &mut std::process::Child) -> std::io::Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let flags = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
    if unsafe { libc::waitid(libc::P_PID, child.id(), &mut info, flags) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { info.si_pid() } != 0)
}

#[cfg(not(target_os = "linux"))]
pub fn shell_exited(child: &mut std::process::Child) -> std::io::Result<bool> {
    child.try_wait().map(|s| s.is_some())
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
    // On Linux the sandbox is required: without it, no command runs.
    if let Some(why) = crate::overlay::confine::refusal() {
        return Err(why);
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
                    whole: None,
                })
            }
        }
    }

    let started = Instant::now();
    // An isolated chat's policy (its overlay's redirect, and hiding), else
    // the one every other agent command gets: nothing redirected, the
    // credential stores hidden, where this machine can (overlay/confine.rs).
    let confine = queued
        .and_then(|q| q.confine.clone())
        .or_else(|| crate::overlay::confine::hiding_if_available(root));
    let mut cmd = shell_command(command, confine.as_ref());
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
    let tree = ProcessTree::of(&child);
    if let Some(job) = &job {
        job.spawned(child.id());
    }

    let whole = queued.is_some_and(|q| q.whole_output);
    let (stdout, out_read) = drain(child.stdout.take().ok_or("no stdout")?, whole);
    let (stderr, err_read) = drain(child.stderr.take().ok_or("no stderr")?, whole);

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
            tree.kill(&mut child);
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    // Reaped only now, after any kill of its tree: see shell_exited.
    let status = child.wait().ok();

    // A process that left the group (setsid, a detached daemon) may still
    // hold the pipes after the kill; don't wait on the readers forever
    // then: what they read so far is the output, cut.
    let killed = timed_out || stopped;
    let join = |handle: thread::JoinHandle<()>, read: Arc<Mutex<Draining>>| -> Drained {
        if killed {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !handle.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
        }
        let ended = handle.is_finished() || !killed;
        if ended {
            let _ = handle.join();
        }
        let read = read.lock().unwrap_or_else(|e| e.into_inner()).clone();
        read.finish(!ended)
    };
    let ((out, out_cut), out_whole) = join(stdout, out_read);
    let ((err, err_cut), err_whole) = join(stderr, err_read);
    let text = |b: Vec<u8>| String::from_utf8_lossy(&b).into_owned();
    let whole = match (out_whole, err_whole) {
        (None, None) => None,
        (o, e) => Some((text(o.unwrap_or_default()), text(e.unwrap_or_default()))),
    };
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
        whole,
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
    crate::rt::spawn_blocking(move || {
        run_blocking(&root, &command, timeout, &stop, queued.as_ref())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A daemon at -500 starts its commands at 0 (5.3). Lowering this
    /// process's score takes privilege: without it, there's nothing to see.
    #[cfg(target_os = "linux")]
    #[test]
    fn commands_start_with_an_ordinary_oom_score() {
        let own = "/proc/self/oom_score_adj";
        if std::fs::write(own, "-500").is_err() {
            return;
        }
        let out = shell_command("cat /proc/self/oom_score_adj", None).output();
        // An isolated chat's too, whichever way it's confined.
        let dir = std::env::temp_dir().join(format!("oom-confined-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("view")).unwrap();
        std::fs::create_dir_all(dir.join("project")).unwrap();
        let c =
            crate::overlay::confine::Confine::redirecting(dir.join("view"), dir.join("project"));
        let confined = crate::overlay::confine::available(&c)
            .then(|| shell_command("cat /proc/self/oom_score_adj", Some(&c)).output());
        let _ = std::fs::write(own, "0");
        let _ = std::fs::remove_dir_all(&dir);
        let out = out.unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "0");
        if let Some(out) = confined {
            let out = out.unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "0", "confined");
        }
    }

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

    /// V07: a check's output past what's kept to show is kept whole for
    /// its failure keys: an error in the middle of 200 KB is among them.
    #[cfg(not(windows))]
    #[test]
    fn a_checks_whole_output_keeps_the_middle_errors_for_its_keys() {
        use crate::agent_run::command_output::failure_keys;
        let dir = std::env::temp_dir();
        let lines = "i=0; while [ $i -lt 2000 ]; do echo \"ok line $i, padded out to well over fifty bytes long\"; i=$((i+1)); done";
        let script = format!(
            "{lines}; echo 'src/mid.ts(3,5): error TS2322: Type string is not number.'; {lines}; \
             echo 'src/end.ts(1,1): error TS2304: Cannot find name x.'; exit 2"
        );
        let queued = Queued::check("whole-output-test", None).whole(true);
        let out = run_blocking(
            dir.to_str().unwrap(),
            &script,
            Duration::from_secs(60),
            &|| false,
            Some(&queued),
        )
        .unwrap();
        assert!(out.stdout.len() + out.stderr.len() < HEAD_BYTES + TAIL_BYTES + 200);
        assert!(
            out.truncated && !out.stdout.contains("src/mid.ts"),
            "shown: cut"
        );
        let (stdout, _) = out.whole.expect("kept whole");
        assert!(stdout.len() > 200_000, "{}", stdout.len());
        let keys = failure_keys("typecheck", &stdout, false);
        assert!(keys.iter().any(|k| k.contains("src/mid.ts")), "{keys:?}");
        assert!(keys.iter().any(|k| k.contains("src/end.ts")), "{keys:?}");
        // What's shown, as before: the end's error only.
        let shown = failure_keys("typecheck", &out.stdout, false);
        assert!(!shown.iter().any(|k| k.contains("src/mid.ts")), "{shown:?}");
        // Not asked for: not kept.
        let plain = run_blocking(
            dir.to_str().unwrap(),
            "echo hi",
            Duration::from_secs(5),
            &|| false,
            None,
        )
        .unwrap();
        assert!(plain.whole.is_none());
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
        // In bubblewrap's sandbox (Linux), the command's pid namespace ends
        // with the shell: nothing it left behind outlives it to hold the
        // output, so the command just ends. Seatbelt (macOS) has no pid
        // namespace, and without a sandbox there's nothing either: the child
        // holds the output until the timeout.
        let sandboxed = crate::overlay::confine::hiding_if_available(&dir).is_some();
        let ends_background = sandboxed && cfg!(target_os = "linux");
        assert_eq!(out.timed_out, !ends_background);
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

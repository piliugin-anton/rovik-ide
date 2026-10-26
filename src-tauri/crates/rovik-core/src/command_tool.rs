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
#[cfg(windows)]
use crate::process::JobObject;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub mod background;

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

impl CommandOutput {
    /// Both streams as one text, stderr first: compilers print to either,
    /// and a failure's lines are more often there.
    pub fn text(&self) -> String {
        joined(&self.stdout, &self.stderr)
    }

    /// [`CommandOutput::text`] of the whole output, when it was kept
    /// (`Queued::whole`).
    pub fn whole_text(&self) -> Option<String> {
        self.whole
            .as_ref()
            .map(|(stdout, stderr)| joined(stdout, stderr))
    }
}

fn joined(stdout: &str, stderr: &str) -> String {
    [stderr.trim(), stdout.trim()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
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
/// e.g. a dev server it ran alive and holding the output pipes. What the
/// shell leaves running ends once it exits, as everything in bubblewrap's
/// sandbox does.
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
    job: Option<JobObject>,
}

impl ProcessTree {
    /// The tree of `child`, just spawned: taken at once, before it has
    /// had the time to start anything outside it.
    #[cfg_attr(not(windows), allow(unused_variables))]
    pub fn of(child: &std::process::Child) -> ProcessTree {
        #[cfg(windows)]
        use std::os::windows::io::AsRawHandle;
        ProcessTree {
            // The child's handle is open while `child` is.
            #[cfg(windows)]
            job: JobObject::of(windows::Win32::Foundation::HANDLE(child.as_raw_handle())),
        }
    }

    /// Kills the shell and everything it started.
    pub fn kill(&self, child: &mut std::process::Child) {
        #[cfg(unix)]
        kill_group(child);
        #[cfg(windows)]
        if !self.job.as_ref().is_some_and(JobObject::kill) {
            let _ = crate::process::no_window(&mut Command::new("taskkill"))
                .args(["/T", "/F", "/PID", &child.id().to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = child.kill();
    }

    /// Ends what the shell, which has exited, left running (a server
    /// started with `&`): everything still in its process group on Unix,
    /// where a process that left the group (setsid, a daemon) is out of
    /// reach, and in its job on Windows.
    #[cfg_attr(not(unix), allow(unused_variables))]
    pub fn end_leftovers(&self, child: &std::process::Child) {
        #[cfg(unix)]
        kill_group(child);
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.kill();
        }
    }
}

/// Kills the process group `child` leads. The group outlives its leader
/// while any member is alive, so this works after the shell has exited
/// too.
#[cfg(unix)]
fn kill_group(child: &std::process::Child) {
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        unsafe { libc::kill(-pid, libc::SIGKILL) };
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

/// Whether a command may run in `root`: the folder is there, and the
/// sandbox Linux requires works. Why not, when it can't.
fn may_run_in(root: &Path) -> Result<(), String> {
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
    Ok(())
}

/// A chat's command (`queued`) admitted to the queue for heavy work
/// (src/workload): its job, and how long it waited for a slot when it
/// was predicted heavy (a wait that isn't part of its time or its
/// timeout).
type Admitted = (
    Option<crate::workload::queue::Job>,
    Option<crate::workload::Waited>,
);

/// `command`, run in `root`, through the queue for heavy work when it's a
/// chat's (`queued`): what [`Admitted`] says, once it may start; None when
/// `stop` came while it waited.
fn admitted(
    root: &Path,
    command: &str,
    stop: &dyn Fn() -> bool,
    queued: Option<&Queued>,
) -> Option<Admitted> {
    let job = queued.map(|q| crate::workload::begin(q.kind.clone(), command, Some(root)));
    let mut waited = None;
    if let (Some(job), Some(q)) = (&job, queued) {
        waited = Some(job.wait_for_slot(stop, &*q.note)?).filter(|w| w.ms > 0);
    }
    Some((job, waited))
}

/// The sandbox a command in `root` runs in: an isolated chat's policy (its
/// overlay's redirect, and hiding), else the one every other agent command
/// gets: nothing redirected, the credential stores hidden, where this
/// machine can (overlay/confine.rs).
fn policy(root: &Path, queued: Option<&Queued>) -> Option<crate::overlay::confine::Confine> {
    queued
        .and_then(|q| q.confine.clone())
        .or_else(|| crate::overlay::confine::hiding_if_available(root))
}

/// `command` as it starts in `root`: through the user's shell, confined by
/// `confine`, with no stdin and what every command's environment has (no
/// pagers or prompts, no colors, its share of the CPU when `job` runs
/// beside others, `queued`'s variables, the host's toolchains, the private
/// network's browser), at the head of a process group of its own. Where
/// its output goes is the caller's to say.
fn prepared(
    root: &Path,
    command: &str,
    confine: Option<&crate::overlay::confine::Confine>,
    job: Option<&crate::workload::queue::Job>,
    queued: Option<&Queued>,
) -> Command {
    let mut cmd = shell_command(command, confine);
    crate::process::no_window(&mut cmd)
        .current_dir(root)
        .stdin(Stdio::null())
        // Tools that page or prompt should just print and exit.
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("CI", "1")
        // Plain text: color codes only cost context and break matching.
        .env("NO_COLOR", "1")
        .env("CARGO_TERM_COLOR", "never");
    for (key, value) in job.map(|j| j.cpu_split_env()).unwrap_or_default() {
        cmd.env(key, value);
    }
    for (key, value) in queued.map(|q| q.env.as_slice()).unwrap_or_default() {
        cmd.env(key, value);
    }
    // A cloud host's toolchains, where the daemon shares them (toolchains):
    // the versions' folders in front of the PATH, and rustup's home. Set
    // here, so a command gets them whether or not it runs confined.
    for (key, value) in crate::toolchains::env_for(root) {
        cmd.env(key, value);
    }
    // In a team server's private network, the network's browser, which
    // opens the chat's pages on the user's computer (network/pages.rs).
    if let Some(network) = confine.and_then(|c| c.network.as_ref()) {
        let chat = queued.and_then(|q| q.kind.owner()).unwrap_or_default();
        network.give_browser(&mut cmd, chat);
    }
    // Its own process group, so a timeout or a stop can take down
    // everything the shell started (a `node server.js`), not just the
    // shell.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

fn run_blocking(
    root: &str,
    command: &str,
    timeout: Duration,
    stop: &dyn Fn() -> bool,
    queued: Option<&Queued>,
) -> Result<CommandOutput, String> {
    let root = Path::new(root);
    may_run_in(root)?;
    let Some((job, waited)) = admitted(root, command, stop, queued) else {
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
        });
    };

    let started = Instant::now();
    let confine = policy(root, queued);
    // One run of the command, from its start to its end.
    let attempt = || -> Result<CommandOutput, String> {
        let mut cmd = prepared(root, command, confine.as_ref(), job.as_ref(), queued);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
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

        // Done when the shell has exited and the pipes are closed. What the
        // shell left running ends with it, so only a process that left its
        // group (setsid, a daemon) can still hold them: that counts as
        // running until the timeout.
        let mut timed_out = false;
        let mut stopped = false;
        let mut exited = false;
        loop {
            if !exited {
                exited = shell_exited(&mut child)
                    .map_err(|e| format!("failed to wait for the command: {e}"))?;
                if exited {
                    tree.end_leftovers(&child);
                }
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
            waited: None,
            whole,
        })
    };
    // A command whose sandbox couldn't be made never started: it starts
    // again (overlay/confine.rs `until_made`).
    let unmade = |out: &CommandOutput| {
        !out.stopped
            && !out.timed_out
            && confine.as_ref().is_some_and(|c| {
                crate::overlay::confine::never_started(c, out.exit_code, &out.stdout, &out.stderr)
            })
    };
    let mut out = crate::overlay::confine::until_made(attempt, unmade)?;
    out.waited = waited;
    Ok(out)
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
        // Time for a busy machine to start the command and print first.
        let out = run_blocking(
            dir.to_str().unwrap(),
            &command,
            Duration::from_secs(3),
            &|| false,
            None,
        )
        .unwrap();
        assert!(out.timed_out);
        assert_eq!(out.stdout.trim(), "started");
        assert!(started.elapsed() < Duration::from_secs(15));
        let ps = Command::new("ps").args(["-eo", "args"]).output().unwrap();
        assert!(
            !String::from_utf8_lossy(&ps.stdout).contains(&marker),
            "the shell's child survived"
        );
    }

    /// On a team server a command runs in the user's private network: it
    /// takes a port another user's server holds on the host, and its
    /// browser asks Rovik, for its chat.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_command_in_the_users_network_takes_a_port_the_host_holds_and_uses_its_browser() {
        let Some((dir, network)) =
            crate::overlay::confine::network::for_test(Path::new("/bin/echo"), &[], None)
        else {
            return;
        };
        if !crate::process::on_path("python3") {
            eprintln!("python3 isn't installed: skipped");
            return;
        }
        let root = dir.0.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let Some(policy) = crate::overlay::confine::hiding_if_available(&root) else {
            eprintln!("no sandbox confines commands here: skipped");
            return;
        };
        let host = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = host.local_addr().unwrap().port();
        let mut queued = Queued::of(
            crate::workload::JobKind::Agent {
                owner: "chat-7".into(),
            },
            None,
        );
        queued.confine = Some(policy.with_network(std::sync::Arc::new(network)));
        let script = format!(
            "echo \"chat=$ROVIK_CHAT\"; xdg-open http://localhost:{port}/; \
             python3 -c 'import socket; s = socket.socket(); s.bind((\"127.0.0.1\", {port})); print(\"took\", {port})'"
        );
        let out = run_blocking(
            root.to_str().unwrap(),
            &script,
            Duration::from_secs(60),
            &|| false,
            Some(&queued),
        )
        .unwrap();
        assert_eq!(out.exit_code, Some(0), "{}{}", out.stdout, out.stderr);
        assert!(out.stdout.contains("chat=chat-7\n"), "{}", out.stdout);
        // The shim ran the opener (here echo) with what it was asked.
        assert!(
            out.stdout
                .contains(&format!("open http://localhost:{port}/\n")),
            "{}",
            out.stdout
        );
        assert!(
            out.stdout.contains(&format!("took {port}")),
            "{}",
            out.stdout
        );
        drop(host);
    }

    /// What a command left running ends with its shell, under any sandbox
    /// as in bubblewrap's, whose pid namespace ends with the shell: it
    /// holds the command's output no longer than that.
    #[cfg(not(windows))]
    #[test]
    fn what_a_command_left_running_ends_with_it_and_holds_no_output() {
        let dir = std::env::temp_dir();
        let started = Instant::now();
        // The shell exits at once; its background child would keep stdout
        // open for 30 s.
        let out = run_blocking(
            dir.to_str().unwrap(),
            "sleep 30 & echo up",
            Duration::from_secs(60),
            &|| false,
            None,
        )
        .unwrap();
        assert!(!out.timed_out);
        assert_eq!(out.stdout.trim(), "up");
        assert_eq!(out.exit_code, Some(0));
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    /// A server left running with `&` and its output sent elsewhere holds
    /// nothing of the command's, and still ends with it, where no pid
    /// namespace ends it (Landlock, Seatbelt, no sandbox) too.
    #[cfg(unix)]
    #[test]
    fn a_server_left_running_with_its_output_elsewhere_ends_with_the_command() {
        let dir = crate::testing::temp_dir();
        let command = "(while :; do echo . >> beat; sleep 0.05; done) > /dev/null 2>&1 & echo up";
        let out = run_blocking(
            dir.0.to_str().unwrap(),
            command,
            Duration::from_secs(60),
            &|| false,
            None,
        )
        .unwrap();
        assert_eq!(out.stdout.trim(), "up");
        assert_eq!(out.exit_code, Some(0));
        assert!(!crate::testing::beats(&dir.0), "it was left behind");
    }

    #[cfg(not(windows))]
    #[test]
    fn stop_kills_the_command() {
        let dir = std::env::temp_dir();
        // Stopped once it has said so, not after a set time: a busy machine
        // may take longer than that to start the sandbox and the shell.
        let said = dir.join(format!("stop-said-{}", std::process::id()));
        let _ = std::fs::remove_file(&said);
        let marker = said.clone();
        let started = Instant::now();
        let stop = move || marker.exists();
        let out = run_blocking(
            dir.to_str().unwrap(),
            &format!("sleep 30 & echo up; touch {}; wait", said.display()),
            Duration::from_secs(60),
            &stop,
            None,
        )
        .unwrap();
        let _ = std::fs::remove_file(&said);
        assert!(out.stopped);
        assert!(!out.timed_out);
        assert_eq!(out.stdout.trim(), "up");
        assert!(started.elapsed() < Duration::from_secs(20));
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

    /// A cloud host's toolchains are in the environment of every command,
    /// confined or not; on a computer nothing changes.
    // A POSIX shell's `$VAR`s: cloud hosts are Linux.
    #[cfg(unix)]
    #[test]
    fn a_cloud_hosts_toolchains_are_in_every_commands_environment() {
        let dir = crate::testing::temp_dir();
        let project = &dir.0;
        let _lock = crate::toolchains::test_lock();
        // Nothing set: the command's own PATH, as before.
        crate::toolchains::unset();
        let plain = run_blocking(
            project.to_str().unwrap(),
            "echo \"$RUSTUP_HOME|$RUSTUP_AUTO_INSTALL\"",
            Duration::from_secs(10),
            &|| false,
            None,
        )
        .unwrap();
        assert_eq!(plain.exit_code, Some(0));
        // Whatever this machine's own are: not the shared ones.
        let (home, auto) = plain.stdout.trim().split_once('|').unwrap();
        assert!(!home.contains("toolchains"), "nothing shared: {home:?}");
        assert_ne!(auto, "0", "nothing shared: {auto:?}");

        let root = dir.0.join("toolchains");
        std::fs::create_dir_all(root.join("rust/cargo/bin")).unwrap();
        std::fs::write(root.join("rust/cargo/bin/cargo"), "").unwrap();
        crate::toolchains::set(crate::toolchains::Shared {
            root: root.clone(),
            caches: None,
            installer: None,
            bin: None,
        });
        let shown = "echo \"$RUSTUP_HOME|$RUSTUP_AUTO_INSTALL|${PATH%%:*}\"";
        // Without a policy (a machine with no sandbox) and confined, as a
        // chat's command runs on a server.
        for confine in [None, crate::overlay::confine::hiding_if_available(project)] {
            let mut queued = Queued::of(crate::workload::JobKind::Terminal, None);
            queued.confine = confine;
            let out = run_blocking(
                project.to_str().unwrap(),
                shown,
                Duration::from_secs(10),
                &|| false,
                Some(&queued),
            )
            .unwrap();
            assert_eq!(out.exit_code, Some(0), "{:?}", out.stderr);
            let said = out.stdout.trim();
            assert_eq!(
                said,
                format!(
                    "{}|0|{}",
                    root.join("rust/rustup").display(),
                    root.join("rust/cargo/bin").display()
                ),
                "{said}"
            );
        }
        crate::toolchains::unset();
    }

    /// A command whose sandbox bubblewrap can't make never ran: it starts
    /// again a moment later, and once five tries couldn't make it, it ends
    /// with bwrap's reason, as before.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_command_whose_sandbox_cant_be_made_starts_again_then_says_why() {
        let dir = crate::testing::temp_dir();
        let project = dir.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        // The chat's view is gone: bwrap can't show it at the project's path.
        let c = crate::overlay::confine::Confine::redirecting(dir.0.join("gone"), project.clone());
        if !crate::overlay::confine::available(&c) {
            eprintln!("no sandbox here shows a view at the project's path: skipped");
            return;
        }
        let mut queued = Queued::of(crate::workload::JobKind::Terminal, None);
        queued.confine = Some(c);
        let out = run_blocking(
            project.to_str().unwrap(),
            "echo ran",
            Duration::from_secs(10),
            &|| false,
            Some(&queued),
        )
        .unwrap();
        assert_eq!(out.exit_code, Some(1), "{}", out.stderr);
        assert_eq!(out.stdout, "");
        assert!(out.stderr.starts_with("bwrap: "), "{}", out.stderr);
        // Five tries, 25, 50, 75 and 100 ms apart.
        assert!(out.duration_ms >= 250, "{} ms", out.duration_ms);
    }
}

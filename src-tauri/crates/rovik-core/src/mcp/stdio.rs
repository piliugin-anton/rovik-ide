//! A command's server, spoken to on its stdin and stdout, one JSON-RPC
//! message per line (docs/design/mcp-plan.md, Q03). It runs as the
//! connectors' harnesses do (connectors/harness.rs): the app's environment
//! fixed for AppImages, in a process group of its own (on Windows, a job),
//! and it ends with everything it started.

use super::rpc::{self, Failure, Incoming};
use crate::agent_run::StopSignal;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

/// How much of its stderr is kept: its end, for an error.
const STDERR_KEPT: usize = 4 * 1024;
/// How long it has to end once its input closed (the spec's way to stop a
/// server), then once asked to.
#[cfg(not(any(test, feature = "testing")))]
const EXIT_GRACE: Duration = Duration::from_millis(1500);
#[cfg(any(test, feature = "testing"))]
const EXIT_GRACE: Duration = Duration::from_millis(300);
/// The most a line it writes may be: a big tool result fits.
const MAX_LINE: usize = 64 << 20;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, Failure>>>>>;
/// Its input, line by line; none once closed. The reader answers the
/// server's own requests through it too, so closing it closes the input.
type Input = Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>;

/// A server's process, while it runs.
pub(super) struct Stdio {
    input: Input,
    /// Rovik's requests waiting for their answers, by id.
    pending: Pending,
    next: AtomicU64,
    /// Why it ended, once it has.
    ended: Arc<Mutex<Option<String>>>,
    stderr: Arc<Mutex<VecDeque<u8>>>,
    child: Arc<Mutex<tokio::process::Child>>,
    pid: Option<u32>,
    /// Its tree, on Windows: the job it went into as it started.
    tree: Arc<crate::process::SessionTree>,
}

/// What `command` runs: the file of that name on the PATH the app's
/// programs get (on Windows, `npx` is `npx.cmd`, which only its full path
/// starts), or the path it is.
fn program(command: &str) -> Result<String, String> {
    if command.contains('/') || command.contains('\\') {
        return Ok(crate::connectors::harness::expand_home(command)
            .to_string_lossy()
            .into_owned());
    }
    crate::process::find_on_path(command)
        .map(|path| path.to_string_lossy().into_owned())
        .ok_or_else(|| format!("{command} isn't installed, or isn't on the PATH."))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl Stdio {
    /// Starts `command` with `args` in `folder`, `env` added to the app's.
    pub(super) fn start(
        command: &str,
        args: &[String],
        env: &[(String, String)],
        folder: Option<&Path>,
    ) -> Result<Stdio, String> {
        let program = program(command)?;
        let mut cmd = tokio::process::Command::from(crate::connectors::harness::command(
            &program, folder, env,
        ));
        cmd.args(args).kill_on_drop(true);
        // Its pipes are the process's runtime's, where they're read
        // (crate::rt), whichever runtime asked for the server.
        let entered = crate::rt::handle().enter();
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Couldn't start {command}: {e}"))?;
        drop(entered);
        let pid = child.id();
        let tree = Arc::new(crate::process::SessionTree::of(pid.unwrap_or(0)));
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let (input, mut lines) = mpsc::unbounded_channel::<String>();
        if let Some(mut stdin) = stdin {
            crate::rt::spawn(async move {
                while let Some(line) = lines.recv().await {
                    if stdin.write_all(line.as_bytes()).await.is_err()
                        || stdin.flush().await.is_err()
                    {
                        break;
                    }
                }
                let _ = stdin.shutdown().await;
            });
        }
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(mut pipe) = stderr_pipe {
            let kept = stderr.clone();
            crate::rt::spawn(async move {
                let mut buf = [0u8; 4096];
                while let Ok(n) = pipe.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let mut kept = lock(&kept);
                    kept.extend(&buf[..n]);
                    let over = kept.len().saturating_sub(STDERR_KEPT);
                    kept.drain(..over);
                }
            });
        }
        let stdio = Stdio {
            input: Arc::new(Mutex::new(Some(input))),
            pending: Arc::default(),
            next: AtomicU64::new(1),
            ended: Arc::default(),
            stderr,
            child: Arc::new(Mutex::new(child)),
            pid,
            tree,
        };
        let Some(stdout) = stdout else {
            return Err(format!("{command}'s output couldn't be read."));
        };
        stdio.read(stdout);
        Ok(stdio)
    }

    /// Reads what it writes: each answer to the request waiting for it, a
    /// request of its own answered (`rpc::answer`), the rest dropped. Once
    /// its output ends, so has it: what waits fails, saying why.
    fn read(&self, stdout: tokio::process::ChildStdout) {
        let input = self.input.clone();
        let pending = self.pending.clone();
        let ended = self.ended.clone();
        let stderr = self.stderr.clone();
        let child = self.child.clone();
        crate::rt::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            loop {
                line.clear();
                match read_line(&mut reader, &mut line).await {
                    Ok(true) => {}
                    Ok(false) | Err(_) => break,
                }
                // Anything that isn't JSON (a server's stray log line) is
                // left out, as the spec says it mustn't be there.
                let Ok(message) = serde_json::from_slice::<Value>(&line) else {
                    continue;
                };
                match rpc::incoming(&message) {
                    Incoming::Response { id, outcome } => {
                        if let Some(waiting) = lock(&pending).remove(&id) {
                            let _ = waiting.send(outcome);
                        }
                    }
                    Incoming::Request { id, method } => {
                        if let Some(input) = lock(&input).as_ref() {
                            let _ = input.send(format!("{}\n", rpc::answer(&id, &method)));
                        }
                    }
                    Incoming::Other => {}
                }
            }
            // Its stderr's last words come a moment after its output ends.
            tokio::time::sleep(Duration::from_millis(100)).await;
            let status = lock(&child).try_wait().ok().flatten();
            let why = ended_text(status, &tail_of(&stderr));
            *lock(&ended) = Some(why.clone());
            for (_, waiting) in lock(&pending).drain() {
                let _ = waiting.send(Err(Failure::Gone(why.clone())));
            }
        });
    }

    /// Why it ended, once it has.
    pub(super) fn ended(&self) -> Option<String> {
        lock(&self.ended).clone()
    }

    /// The end of what it wrote to stderr.
    pub(super) fn stderr(&self) -> String {
        tail_of(&self.stderr)
    }

    fn send(&self, message: &Value) -> Result<(), Failure> {
        let line = format!("{message}\n");
        match lock(&self.input).as_ref() {
            Some(input) if input.send(line).is_ok() => Ok(()),
            _ => Err(Failure::Gone(
                self.ended()
                    .unwrap_or_else(|| "the server's input is closed".into()),
            )),
        }
    }

    /// Sends a notification.
    pub(super) fn notify(&self, method: &str, params: Value) {
        let _ = self.send(&rpc::notification(method, params));
    }

    /// Sends the request, and waits up to `within`, or until `stop`, for
    /// its answer. One that doesn't come is cancelled
    /// (`notifications/cancelled`).
    pub(super) async fn request(
        &self,
        method: &str,
        params: Value,
        within: Duration,
        stop: Option<&StopSignal>,
    ) -> Result<Value, Failure> {
        if let Some(why) = self.ended() {
            return Err(Failure::Gone(why));
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = oneshot::channel();
        lock(&self.pending).insert(id, reply);
        // Ended meanwhile: the reader, which marks the end before it fails
        // what waits, may have done so before this request was waiting.
        if let Some(why) = self.ended() {
            lock(&self.pending).remove(&id);
            return Err(Failure::Gone(why));
        }
        if let Err(failure) = self.send(&rpc::request(id, method, params)) {
            lock(&self.pending).remove(&id);
            return Err(failure);
        }
        let waited = super::wait_for(answer, within, stop).await;
        let failure = match waited {
            Ok(Ok(outcome)) => return outcome,
            // The reader went: the process ended.
            Ok(Err(_)) => {
                return Err(Failure::Gone(
                    self.ended()
                        .unwrap_or_else(|| "the server's process ended".into()),
                ))
            }
            Err(failure) => failure,
        };
        lock(&self.pending).remove(&id);
        let reason = match failure {
            Failure::Stopped => "stopped by the user",
            _ => "it took too long",
        };
        self.notify(
            "notifications/cancelled",
            serde_json::json!({ "requestId": id, "reason": reason }),
        );
        Err(failure)
    }

    /// Ends it as the spec says: its input closed, then asked to end, then
    /// killed with all it started.
    pub(super) async fn stop(&self) {
        lock(&self.input).take();
        if !self.exits_within(EXIT_GRACE).await {
            crate::process::kill_group(self.pid, &mut lock(&self.child), false);
            if !self.exits_within(EXIT_GRACE).await {
                crate::process::kill_group(self.pid, &mut lock(&self.child), true);
            }
        }
        // What it left running: on Windows its job's, which waits a moment
        // for them, off the async threads; on Unix its group was signalled.
        let tree = self.tree.clone();
        let _ = crate::rt::spawn_blocking(move || tree.end()).await;
    }

    /// Whether it has exited within `within`.
    async fn exits_within(&self, within: Duration) -> bool {
        let until = tokio::time::Instant::now() + within;
        loop {
            if matches!(lock(&self.child).try_wait(), Ok(Some(_))) {
                return true;
            }
            if tokio::time::Instant::now() >= until {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for Stdio {
    /// A server not stopped first (`stop`) is killed with its group, and
    /// on Windows its job's leftovers end on a thread of their own, as that
    /// waits a moment for them.
    fn drop(&mut self) {
        let mut child = lock(&self.child);
        if matches!(child.try_wait(), Ok(None)) {
            crate::process::kill_group(self.pid, &mut child, true);
        }
        if cfg!(windows) {
            let tree = self.tree.clone();
            std::thread::spawn(move || tree.end());
        }
    }
}

/// Reads one line into `line`, its newline left out, at most [`MAX_LINE`]
/// bytes of it kept: false once the output ended.
async fn read_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> std::io::Result<bool> {
    loop {
        let buf = reader.fill_buf().await?;
        if buf.is_empty() {
            return Ok(!line.is_empty());
        }
        let (taken, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(at) => (at + 1, true),
            None => (buf.len(), false),
        };
        let room = MAX_LINE.saturating_sub(line.len());
        line.extend_from_slice(&buf[..taken.min(room)]);
        reader.consume(taken);
        if done {
            while line.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
                line.pop();
            }
            return Ok(true);
        }
    }
}

fn tail_of(stderr: &Mutex<VecDeque<u8>>) -> String {
    let kept: Vec<u8> = lock(stderr).iter().copied().collect();
    String::from_utf8_lossy(&kept).trim().to_string()
}

/// Why a server's process ended, for its error: its exit, and the end of
/// its stderr.
fn ended_text(status: Option<std::process::ExitStatus>, stderr: &str) -> String {
    let exit = match status.and_then(|s| s.code()) {
        Some(code) => format!("The server exited with code {code}"),
        None => "The server's process ended".to_string(),
    };
    if stderr.is_empty() {
        format!("{exit}.")
    } else {
        format!("{exit}. It said:\n{stderr}")
    }
}

//! A connector's "Set up" (Settings → Connectors): installing its program,
//! signing it in or out, on the computer asked (`SetupOp`). What a step
//! runs is the connector's own ([`super::Connector::setup_command`]); the
//! rest is here, the same for every connector.
//!
//! A step runs as a job of its own, followed by its id: started, polled,
//! given a line, stopped. The same calls follow one here and on a cloud
//! host, whose user's own daemon runs it as that user. Its output is kept
//! for the user to read, the first web address it prints is the page to
//! open (a sign-in), and a line the user types (the code that page shows)
//! goes to its input.

use super::{connector, ConnectorKind, ProbeRequestArgs, SetupAction, SetupJob, SetupOp};
use crate::agent_run::command_output::clean_output;
use std::collections::HashMap;
use std::process::ExitStatus;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin};
use tokio::sync::watch;

/// The longest a step runs: an installer's download, or a sign-in the
/// user takes their time over.
const LIMIT: Duration = Duration::from_secs(15 * 60);
/// How long a finished step is kept for a poll that comes late.
const KEPT: Duration = Duration::from_secs(10 * 60);
/// Of a step's output, the latest bytes kept; of its errors, for the
/// reason it failed.
const OUTPUT_KEPT: usize = 16 * 1024;
const ERRORS_KEPT: usize = 2 * 1024;
/// After the program exits, how long what it printed last may take.
const LAST_OUTPUT: Duration = Duration::from_secs(2);

/// What a setup step runs: a connector's answer to `setup_command`.
#[derive(Debug, Clone, PartialEq)]
pub struct SetupCommand {
    pub program: String,
    pub args: Vec<String>,
    /// Besides the environment the IDE's programs get.
    pub env: Vec<(String, String)>,
    /// For the user: the command as they'd type it.
    pub shown: String,
    /// Its sign-in page asks for a code it prints (`SetupJob::device_code`).
    pub device_code: bool,
}

impl SetupCommand {
    /// `program` with `args`.
    pub fn program(program: impl Into<String>, args: &[&str]) -> Self {
        let program = program.into();
        let command = Self {
            shown: program.clone(),
            program,
            args: Vec::new(),
            env: Vec::new(),
            device_code: false,
        };
        args.iter().fold(command, |command, arg| command.arg(arg))
    }

    /// A line for the system's shell: `sh -c` on Unix, PowerShell on
    /// Windows, where installers are given as PowerShell (`irm … | iex`).
    pub fn shell(line: &str) -> Self {
        let (program, args): (&str, &[&str]) = if cfg!(windows) {
            (
                "powershell",
                &[
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                ],
            )
        } else {
            ("sh", &["-c"])
        };
        Self {
            program: program.into(),
            args: args
                .iter()
                .map(|a| a.to_string())
                .chain([line.into()])
                .collect(),
            env: Vec::new(),
            shown: line.into(),
            device_code: false,
        }
    }

    pub fn arg(mut self, arg: &str) -> Self {
        self.args.push(arg.into());
        self.shown = format!("{} {arg}", self.shown);
        self
    }

    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self
    }

    /// Its sign-in page asks for the code it prints: the user enters it
    /// there, rather than pasting one back.
    pub fn with_device_code(mut self) -> Self {
        self.device_code = true;
        self
    }
}

/// A step's answer to each `SetupOp`: the step as it then stands.
pub async fn setup(op: SetupOp) -> Result<SetupJob, String> {
    match op {
        SetupOp::Start { args, action } => {
            let command = command_for(&args, action)?;
            start(args.kind, action, command)
        }
        SetupOp::Poll { id } => Ok(job(&id)?.snapshot(&id)),
        SetupOp::Send { id, text } => {
            let job = job(&id)?;
            job.send(&text).await?;
            Ok(job.snapshot(&id))
        }
        SetupOp::Cancel { id } => {
            let job = job(&id)?;
            job.stop.send_replace(true);
            Ok(job.snapshot(&id))
        }
    }
}

/// What `action` runs for the connector `args` names, or why it can't.
fn command_for(args: &ProbeRequestArgs, action: SetupAction) -> Result<SetupCommand, String> {
    connector(args.kind)
        .setup_command(action, args)?
        .ok_or_else(|| {
            let what = match action {
                SetupAction::Install => "be installed",
                SetupAction::Login => "be signed in",
                SetupAction::Logout => "be signed out",
                SetupAction::Update => "be updated",
            };
            format!("{} can't {what} from Rovik.", args.kind.title())
        })
}

/// A step that runs, or ran.
struct Job {
    kind: ConnectorKind,
    action: SetupAction,
    shown: String,
    device_code: bool,
    state: Mutex<State>,
    /// Its input, while it runs.
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    stop: watch::Sender<bool>,
}

#[derive(Default)]
struct State {
    /// The latest of what it printed, as printed: both its outputs.
    output: Vec<u8>,
    /// The latest of what it printed on stderr.
    errors: Vec<u8>,
    /// When it ended.
    ended: Option<Instant>,
    error: Option<String>,
}

static JOBS: LazyLock<Mutex<HashMap<String, Arc<Job>>>> = LazyLock::new(Default::default);

fn jobs() -> MutexGuard<'static, HashMap<String, Arc<Job>>> {
    JOBS.lock().unwrap_or_else(|e| e.into_inner())
}

fn job(id: &str) -> Result<Arc<Job>, String> {
    jobs().get(id).cloned().ok_or_else(|| {
        "That setup step is gone: it ended a while ago, or Rovik restarted.".to_string()
    })
}

/// Starts `command` as a step of `kind`'s, stopping one of its still
/// running: a connector has one step at a time.
fn start(
    kind: ConnectorKind,
    action: SetupAction,
    command: SetupCommand,
) -> Result<SetupJob, String> {
    let cmd = super::harness::command(&command.program, None, &command.env);
    let mut cmd = tokio::process::Command::from(cmd);
    cmd.args(&command.args).kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Couldn't run {}: {e}", command.program))?;
    let (stop, stopped) = watch::channel(false);
    let job = Arc::new(Job {
        kind,
        action,
        shown: command.shown,
        device_code: command.device_code,
        state: Mutex::default(),
        stdin: tokio::sync::Mutex::new(child.stdin.take()),
        stop,
    });
    let id = uuid::Uuid::new_v4().to_string();
    {
        let mut jobs = jobs();
        jobs.retain(|_, j| j.kept());
        for other in jobs.values().filter(|j| j.kind == kind) {
            other.stop.send_replace(true);
        }
        jobs.insert(id.clone(), job.clone());
    }
    tokio::spawn(follow(job.clone(), child, stopped));
    Ok(job.snapshot(&id))
}

/// Runs the step to its end: its exit, a Stop, or the time limit.
async fn follow(job: Arc<Job>, mut child: Child, mut stopped: watch::Receiver<bool>) {
    let pid = child.id();
    let error = tokio::select! {
        status = exit(&job, &mut child) => match status {
            Ok(status) if status.success() => None,
            Ok(status) => Some(job.failure(status)),
            Err(e) => Some(format!("Couldn't follow it: {e}")),
        },
        _ = stopped.wait_for(|stop| *stop) => Some("Stopped.".to_string()),
        _ = tokio::time::sleep(LIMIT) => {
            Some(format!("Stopped: it ran for {} minutes.", LIMIT.as_secs() / 60))
        }
    };
    if error.is_some() {
        crate::process::kill_group(pid, &mut child, true);
        let _ = child.wait().await;
    }
    *job.stdin.lock().await = None;
    let mut state = job.state();
    state.error = error;
    state.ended = Some(Instant::now());
}

/// The program's exit, once what it printed is read.
async fn exit(job: &Arc<Job>, child: &mut Child) -> std::io::Result<ExitStatus> {
    let reading = tokio::spawn({
        let (job, stdout, stderr) = (job.clone(), child.stdout.take(), child.stderr.take());
        async move { tokio::join!(job.read(stdout, false), job.read(stderr, true)) }
    });
    let status = child.wait().await;
    // What it started may keep its outputs open: what came is enough.
    let _ = tokio::time::timeout(LAST_OUTPUT, reading).await;
    status
}

impl Job {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Running, or ended not long ago.
    fn kept(&self) -> bool {
        self.state().ended.is_none_or(|t| t.elapsed() < KEPT)
    }

    /// Keeps what `pipe` prints, its stderr's also apart.
    async fn read(&self, pipe: Option<impl AsyncRead + Unpin>, stderr: bool) {
        let Some(mut pipe) = pipe else { return };
        let mut buf = [0u8; 4096];
        while let Ok(n @ 1..) = pipe.read(&mut buf).await {
            let mut state = self.state();
            keep_latest(&mut state.output, &buf[..n], OUTPUT_KEPT);
            if stderr {
                keep_latest(&mut state.errors, &buf[..n], ERRORS_KEPT);
            }
        }
    }

    /// A line for the program: what the user typed, as one line.
    async fn send(&self, text: &str) -> Result<(), String> {
        let text = text.trim();
        if text.is_empty() || text.contains('\n') {
            return Err("Give one line.".into());
        }
        let mut stdin = self.stdin.lock().await;
        let pipe = stdin
            .as_mut()
            .ok_or("It has ended: it takes nothing now.")?;
        let sent = async {
            pipe.write_all(format!("{text}\n").as_bytes()).await?;
            pipe.flush().await
        };
        sent.await
            .map_err(|e| format!("Couldn't give it the line: {e}"))
    }

    /// Why it failed: the last error it printed, else the last thing it
    /// printed, else its exit status.
    fn failure(&self, status: ExitStatus) -> String {
        let state = self.state();
        let said = [&state.errors, &state.output]
            .into_iter()
            .find_map(|bytes| last_line(&clean_output(&String::from_utf8_lossy(bytes))));
        said.unwrap_or_else(|| format!("It ended with {status}."))
    }

    fn output(&self) -> String {
        clean_output(&String::from_utf8_lossy(&self.state().output))
    }

    fn snapshot(&self, id: &str) -> SetupJob {
        let output = self.output();
        let state = self.state();
        SetupJob {
            id: id.to_string(),
            action: self.action,
            command: self.shown.clone(),
            url: first_url(&output),
            device_code: self.device_code,
            output,
            done: state.ended.is_some(),
            error: state.error.clone(),
        }
    }
}

/// Appends `bytes` to `kept`, keeping its latest `max`.
fn keep_latest(kept: &mut Vec<u8>, bytes: &[u8], max: usize) {
    kept.extend_from_slice(bytes);
    let extra = kept.len().saturating_sub(max);
    kept.drain(..extra);
}

fn last_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(str::to_string)
}

/// The first `https://` address in `text`, without the punctuation of the
/// sentence around it.
fn first_url(text: &str) -> Option<String> {
    let start = text.find("https://")?;
    let url = text[start..]
        .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`'))
        .next()?
        .trim_end_matches(['.', ',', ';', ':', ')', ']', '!', '?']);
    (url.len() > "https://".len()).then(|| url.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Steps share one table, and a connector one step at a time: these
    /// tests take turns.
    static TURNS: Mutex<()> = Mutex::new(());

    fn turn() -> MutexGuard<'static, ()> {
        TURNS.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Polls the step until `until` holds of it, or 10 s pass.
    async fn wait(id: &str, until: impl Fn(&SetupJob) -> bool) -> SetupJob {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let job = setup(SetupOp::Poll { id: id.into() }).await.unwrap();
            if until(&job) || Instant::now() > deadline {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn run(line: &str) -> SetupJob {
        start(
            ConnectorKind::ClaudeCode,
            SetupAction::Login,
            SetupCommand::shell(line),
        )
        .unwrap()
    }

    #[test]
    fn a_sign_in_shows_its_page_and_takes_the_code() {
        let _turn = turn();
        crate::rt::block_on(async {
            let job = run(
                "printf 'Open https://example.com/sign-in?code=true. Then\\nPaste code here > '; \
                 read code; echo \"got $code\"",
            );
            assert!(job.command.starts_with("printf 'Open"), "{}", job.command);
            let job = wait(&job.id, |j| j.url.is_some()).await;
            assert_eq!(
                job.url.as_deref(),
                Some("https://example.com/sign-in?code=true")
            );
            assert!(!job.done);
            setup(SetupOp::Send {
                id: job.id.clone(),
                text: " abc#xyz \n".into(),
            })
            .await
            .unwrap();
            let job = wait(&job.id, |j| j.done).await;
            assert!(job.output.contains("got abc#xyz"), "{}", job.output);
            assert_eq!(job.error, None);
            let late = setup(SetupOp::Send {
                id: job.id.clone(),
                text: "more".into(),
            })
            .await;
            assert!(late.unwrap_err().contains("ended"));
        });
    }

    #[test]
    fn a_failure_says_what_the_program_said_last() {
        let _turn = turn();
        crate::rt::block_on(async {
            let job = run("echo working; echo 'curl: (6) Could not resolve host' >&2; exit 3");
            let job = wait(&job.id, |j| j.done).await;
            assert_eq!(
                job.error.as_deref(),
                Some("curl: (6) Could not resolve host")
            );
            assert!(job.output.contains("working"));
        });
    }

    #[test]
    fn a_step_stops_when_asked_and_when_another_starts() {
        let _turn = turn();
        crate::rt::block_on(async {
            let first = run("sleep 30");
            let cancelled = setup(SetupOp::Cancel {
                id: first.id.clone(),
            })
            .await
            .unwrap();
            assert_eq!(cancelled.id, first.id);
            let first = wait(&first.id, |j| j.done).await;
            assert_eq!(first.error.as_deref(), Some("Stopped."));

            let second = run("sleep 30");
            let third = run("echo third");
            let second = wait(&second.id, |j| j.done).await;
            assert_eq!(second.error.as_deref(), Some("Stopped."));
            let third = wait(&third.id, |j| j.done).await;
            assert_eq!(third.error, None);
        });
    }

    #[test]
    fn an_unknown_step_is_an_error() {
        crate::rt::block_on(async {
            let err = setup(SetupOp::Poll { id: "nope".into() })
                .await
                .unwrap_err();
            assert!(err.contains("gone"), "{err}");
        });
    }

    #[test]
    fn addresses_are_found_without_their_sentence() {
        assert_eq!(
            first_url("visit: https://a.dev/x?y=1&z=2.\n").as_deref(),
            Some("https://a.dev/x?y=1&z=2")
        );
        assert_eq!(
            first_url("see (https://a.dev/p), then").as_deref(),
            Some("https://a.dev/p")
        );
        assert_eq!(first_url("http://a.dev https:// none"), None);
        assert_eq!(first_url("nothing here"), None);
    }

    /// A step whose page asks for the code it printed says so as it runs.
    #[test]
    fn a_device_code_sign_in_says_so() {
        let _turn = turn();
        crate::rt::block_on(async {
            let command = SetupCommand::shell(
                "printf 'Open https://example.com/device\\nEnter this one-time code\\n   ABCD-1234\\n'",
            )
            .with_device_code();
            let job = start(ConnectorKind::Codex, SetupAction::Login, command).unwrap();
            assert!(job.device_code);
            let job = wait(&job.id, |j| j.done).await;
            assert_eq!(job.url.as_deref(), Some("https://example.com/device"));
            assert!(job.output.contains("ABCD-1234"), "{}", job.output);
            assert!(job.device_code);
            assert!(!run("true").device_code, "a sign-in pasted back");
        });
    }

    #[test]
    fn commands_read_as_typed() {
        let login =
            SetupCommand::program("/home/a/.local/bin/claude", &["auth", "login"]).arg("--console");
        assert_eq!(
            login.shown,
            "/home/a/.local/bin/claude auth login --console"
        );
        assert_eq!(login.args, ["auth", "login", "--console"]);
        let install = SetupCommand::shell("curl -fsSL https://x.dev/i.sh | bash");
        assert_eq!(install.shown, "curl -fsSL https://x.dev/i.sh | bash");
        assert_eq!(install.program, "sh");
        assert_eq!(install.args, ["-c", "curl -fsSL https://x.dev/i.sh | bash"]);
    }
}

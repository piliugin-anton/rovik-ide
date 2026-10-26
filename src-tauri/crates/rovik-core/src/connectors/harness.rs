//! What every harness's run shares (connectors/mod.rs): its program found,
//! run in the project folder, its output read line by line into a chat's
//! events by the connector's own parser ([`Lines`]), stopped or timed out,
//! and told as an error when it ended without saying how. A connector
//! brings the arguments, the environment and the parser: claude_code.rs's
//! program reads its message and runs to its end; codex/'s speaks a
//! protocol on its input as it runs ([`Process::spawn_with_input`]).

use super::harness_error;
use super::mcp::McpCall;
use super::ConnectorKind;
use crate::agent::ChatStreamEvent;
use crate::chat_request::{ChatError, ErrorKind};
use crate::overlay::confine::network::Network;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};

/// The program's first line is due within this, however slow the start.
pub(super) const MIN_FIRST_LINE: Duration = Duration::from_secs(120);
/// Between lines once it runs: a long command of its own prints nothing.
const IDLE: Duration = Duration::from_secs(20 * 60);
/// After a Stop, how long the program gets to end before it's killed.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// Of its stderr, kept for an error's details.
const STDERR_KEPT: usize = 16 * 1024;
/// A program's exit status after SIGTERM (128 + 15).
const EXIT_TERMINATED: i32 = 143;

/// A connector's reading of its program's output, one line at a time.
pub(super) trait Lines {
    /// One line of output, as the chat's events it makes.
    fn on_line(&mut self, line: &str, emit: &mut dyn FnMut(ChatStreamEvent));

    /// The program stopped for Rovik to run tool calls (Rovik's tools
    /// mode): the turn ends here, and the program waits.
    fn yielded(&self) -> bool {
        false
    }

    /// The turn is over, though the program still runs: one that serves a
    /// protocol stays up until its input closes.
    fn finished(&self) -> bool {
        false
    }

    /// The program waits on Rovik's answer to something it asked on its
    /// output (an approval the user takes their time over): it's silent,
    /// not stalled.
    fn waiting(&self) -> bool {
        false
    }
}

/// Where a run's input goes while it runs: each string is written to the
/// program's stdin as it is, and the input closes once every sender is gone.
pub(super) type Input = mpsc::UnboundedSender<String>;

/// The program a connector runs: `setting` as given when it names a path,
/// else `name` (when the setting is empty) found on the PATH the IDE's
/// programs get, else in one of `places`, where it's installed. The error
/// says how to install it: `install`.
pub fn resolve_program(
    setting: &str,
    name: &str,
    places: &[PathBuf],
    install: &str,
) -> Result<String, String> {
    let program = match setting.trim() {
        "" => name,
        given => given,
    };
    if program.contains('/') || program.contains('\\') {
        let path = expand_home(program);
        return if path.is_file() {
            Ok(path.to_string_lossy().into_owned())
        } else {
            Err(format!("{}: no such program.", path.display()))
        };
    }
    if crate::process::on_path(program) {
        return Ok(program.to_string());
    }
    if program == name {
        if let Some(found) = places.iter().find(|p| p.is_file()) {
            return Ok(found.to_string_lossy().into_owned());
        }
    }
    Err(format!(
        "{program} isn't installed, or isn't on the PATH. {install}, or name the \
         program's path in the connector's settings."
    ))
}

/// Where `name` is installed on Linux and macOS when it isn't on the PATH:
/// the user's `~/.local/bin` (the native installers'), then the system's
/// `bin` folders (a package's, a cloud host's), Rovik's and Homebrew's.
pub(super) fn unix_places(name: &str) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/bin"));
    let dirs = [
        "/usr/local/bin",
        "/usr/bin",
        "/opt/rovik/bin",
        "/opt/homebrew/bin",
    ];
    home.into_iter()
        .chain(dirs.iter().map(PathBuf::from))
        .map(|dir| dir.join(name))
        .collect()
}

/// `~/…` as the user's home.
pub fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// Splits extra arguments as a shell would, with quotes and backslashes.
pub fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut any = false;
    for c in text.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        match (quote, c) {
            (None, '\\') | (Some('"'), '\\') => escaped = true,
            (None, '"') | (None, '\'') => {
                quote = Some(c);
                any = true;
            }
            (Some(q), c) if q == c => quote = None,
            (None, c) if c.is_whitespace() => {
                if any || !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                    any = false;
                }
            }
            (_, c) => current.push(c),
        }
    }
    if any || !current.is_empty() {
        args.push(current);
    }
    args
}

/// A harness's program as Rovik runs it: in `cwd` with the toolchains a
/// server shares there, `env` added, its outputs piped, no console window,
/// and a process group of its own, so a Stop ends what it started too.
pub(super) fn command(
    program: &str,
    cwd: Option<&Path>,
    env: &[(String, String)],
) -> std::process::Command {
    let mut cmd = crate::process::command(program);
    crate::process::no_window(&mut cmd);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
        // A server's shared toolchains, as the agent's own commands get them.
        for (key, value) in crate::toolchains::env_for(cwd) {
            cmd.env(key, value);
        }
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::process::own_group(&mut cmd);
    cmd
}

/// The tail of stderr, for an error's details.
async fn read_stderr(stderr: Option<tokio::process::ChildStderr>) -> String {
    let Some(mut stderr) = stderr else {
        return String::new();
    };
    let mut buf = Vec::new();
    let _ = stderr.read_to_end(&mut buf).await;
    let text = String::from_utf8_lossy(&buf);
    let kept: String = text
        .chars()
        .rev()
        .take(STDERR_KEPT)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    kept.trim().to_string()
}

/// A private temporary file (a system prompt, an MCP configuration with
/// its token), gone with the run it's for.
pub(super) struct TempFile(PathBuf);

impl TempFile {
    pub(super) fn write(what: &str, extension: &str, text: &str) -> Result<TempFile, String> {
        let path = std::env::temp_dir().join(format!(
            "rovik-harness-{what}-{}.{extension}",
            uuid::Uuid::new_v4().simple()
        ));
        crate::atomic_file::write_atomic_private(&path, text.as_bytes())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(TempFile(path))
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A harness's run: its output's lines as they come, its stderr, and the
/// files it reads, which go with it. Dropped while it runs, it's killed
/// with everything it started.
pub(super) struct Process {
    child: tokio::process::Child,
    pid: Option<u32>,
    lines: mpsc::UnboundedReceiver<String>,
    stderr: Option<tokio::task::JoinHandle<String>>,
    got_line: bool,
    first_line_within: Duration,
    /// Its input while it stays open (`spawn_with_input`).
    input: Option<Input>,
    _files: Vec<TempFile>,
}

impl Process {
    /// Starts `program`, the turn's message on its stdin, which closes once
    /// written: in `network` for a chat when given (a team server's private
    /// network, with its browser for that chat: overlay/confine/network.rs).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn spawn(
        program: &str,
        args: &[String],
        env: &[(String, String)],
        cwd: Option<&Path>,
        network: Option<(&Network, &str)>,
        prompt: String,
        first_line_within: Duration,
        files: Vec<TempFile>,
    ) -> Result<Process, String> {
        let mut process =
            Self::spawn_with_input(program, args, env, cwd, network, first_line_within, files)?;
        if let Some(input) = process.input.take() {
            let _ = input.send(prompt);
        }
        Ok(process)
    }

    /// Starts `program` as `spawn` does, its stdin open for what the run
    /// sends it as it goes (`input`): a protocol's requests and answers.
    /// It closes with `close_input`, and as the run finishes.
    pub(super) fn spawn_with_input(
        program: &str,
        args: &[String],
        env: &[(String, String)],
        cwd: Option<&Path>,
        network: Option<(&Network, &str)>,
        first_line_within: Duration,
        files: Vec<TempFile>,
    ) -> Result<Process, String> {
        let mut program_cmd = command(program, cwd, env);
        if let Some((network, chat)) = network {
            network.join(&mut program_cmd);
            network.give_browser(&mut program_cmd, chat);
        }
        let mut cmd = tokio::process::Command::from(program_cmd);
        cmd.args(args).kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Couldn't start {program}: {e}"))?;
        // What the run sends, written in order; the input closes once
        // every sender is gone.
        let (input, mut sent) = mpsc::unbounded_channel::<String>();
        if let Some(mut stdin) = child.stdin.take() {
            tokio::spawn(async move {
                while let Some(text) = sent.recv().await {
                    if stdin.write_all(text.as_bytes()).await.is_err() {
                        break;
                    }
                    let _ = stdin.flush().await;
                }
                let _ = stdin.shutdown().await;
            });
        }
        let stderr = Some(tokio::spawn(read_stderr(child.stderr.take())));
        let Some(stdout) = child.stdout.take() else {
            return Err(format!("{program}'s output couldn't be read."));
        };
        let (sender, lines) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            loop {
                match reader.next_line().await {
                    Ok(Some(line)) => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("harness: reading its output: {e}");
                        break;
                    }
                }
            }
        });
        Ok(Process {
            pid: child.id(),
            child,
            lines,
            stderr,
            got_line: false,
            first_line_within,
            input: Some(input),
            _files: files,
        })
    }

    /// A sender of what goes to its input, while it's open.
    pub(super) fn input(&self) -> Option<Input> {
        self.input.clone()
    }

    /// Closes its input once what was sent is written (a sender still held
    /// elsewhere keeps it open until it goes too).
    pub(super) fn close_input(&mut self) {
        self.input = None;
    }

    /// The next line it printed; none once its output has ended.
    pub(super) async fn next_line(&mut self) -> Option<String> {
        let line = self.lines.recv().await;
        self.got_line |= line.is_some();
        line
    }

    fn kill(&mut self, force: bool) {
        crate::process::kill_group(self.pid, &mut self.child, force);
    }

    /// How it ended: its exit code (none when killed) and its stderr's end.
    /// Its input closes first, which ends one that serves it; what it left
    /// running is ended.
    pub(super) async fn finish(&mut self) -> (Option<i32>, String) {
        self.close_input();
        let status = tokio::time::timeout(STOP_GRACE, self.child.wait()).await;
        let code = match status {
            Ok(Ok(status)) => status.code(),
            _ => None,
        };
        // Anything of the group still running, and its hold on stderr.
        crate::process::kill_group(self.pid, &mut self.child, true);
        let stderr = match self.stderr.take() {
            Some(task) => tokio::time::timeout(STOP_GRACE, task)
                .await
                .ok()
                .and_then(|r| r.ok())
                .unwrap_or_default(),
            None => String::new(),
        };
        (code, stderr)
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.kill(true);
        }
    }
}

/// Why `pump` returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stop {
    /// The output ended: the program is done.
    Ended,
    /// Rovik's tools mode: a message made calls to them, which Rovik runs.
    Yielded,
    /// Stop: it was ended.
    Stopped,
    /// It printed nothing for too long, and was ended.
    Stalled,
}

/// Reads the run's lines into events until it ends (or its parser says the
/// turn is over), makes Rovik's tool calls, is stopped or stalls; the MCP
/// calls that come meanwhile go to `on_call`.
pub(super) async fn pump(
    process: &mut Process,
    parser: &mut (impl Lines + ?Sized),
    mut calls: Option<&mut mpsc::UnboundedReceiver<McpCall>>,
    on_call: &mut (dyn FnMut(McpCall) + Send),
    cancel: &mut watch::Receiver<bool>,
    emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
) -> Stop {
    let mut stopping = false;
    // A call to Rovik's server waits on Rovik (an approval the user takes
    // their time over): the program is silent, not stalled, until its
    // next line.
    let mut waiting = false;
    loop {
        let timeout = if process.got_line {
            IDLE
        } else {
            process.first_line_within
        };
        let call = async {
            match calls.as_deref_mut() {
                Some(calls) => calls.recv().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            line = process.lines.recv() => match line {
                Some(line) => {
                    process.got_line = true;
                    waiting = false;
                    parser.on_line(&line, emit);
                    if parser.yielded() && !stopping {
                        return Stop::Yielded;
                    }
                    if parser.finished() && !stopping {
                        return Stop::Ended;
                    }
                }
                None => return if stopping { Stop::Stopped } else { Stop::Ended },
            },
            Some(call) = call => {
                waiting = true;
                on_call(call);
            }
            _ = crate::chat_request::cancelled(cancel), if !stopping => {
                stopping = true;
                process.kill(false);
            }
            _ = tokio::time::sleep(STOP_GRACE), if stopping => {
                process.kill(true);
                return Stop::Stopped;
            }
            _ = tokio::time::sleep(timeout), if !stopping && !waiting && !parser.waiting() => {
                process.kill(true);
                return Stop::Stalled;
            }
        }
    }
}

/// Runs `program` once to its end, `stdin` its input, within `within`:
/// its version, its sign-in, a test request.
pub(super) async fn output(
    program: &str,
    args: &[String],
    env: &[(String, String)],
    stdin: &str,
    within: Duration,
) -> Result<std::process::Output, String> {
    let mut cmd = tokio::process::Command::from(command(program, None, env));
    cmd.args(args).kill_on_drop(true);
    if stdin.is_empty() {
        cmd.stdin(Stdio::null());
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Couldn't run {program}: {e}"))?;
    if let Some(mut pipe) = child.stdin.take() {
        let _ = pipe.write_all(stdin.as_bytes()).await;
        let _ = pipe.shutdown().await;
    }
    let shown = std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    tokio::time::timeout(within, child.wait_with_output())
        .await
        .map_err(|_| format!("`{shown}` didn't answer in {} s.", within.as_secs()))?
        .map_err(|e| format!("Couldn't run {program}: {e}"))
}

/// Runs `program` with `input` and reads its output's lines until `pick`
/// finds its answer in one: a request of its control protocol (the
/// commands it takes) answered, without a turn of the model. The program
/// is ended once it answered, or after `within`.
pub(super) async fn answer<T>(
    program: &str,
    args: &[String],
    env: &[(String, String)],
    cwd: &Path,
    input: String,
    within: Duration,
    mut pick: impl FnMut(&str) -> Option<T>,
) -> Result<T, String> {
    let mut process = Process::spawn(
        program,
        args,
        env,
        Some(cwd),
        None,
        input,
        within,
        Vec::new(),
    )?;
    let deadline = tokio::time::sleep(within);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            line = process.lines.recv() => match line {
                Some(line) => {
                    if let Some(found) = pick(&line) {
                        process.kill(true);
                        return Ok(found);
                    }
                }
                None => {
                    let (_, stderr) = process.finish().await;
                    let why = stderr.lines().last().unwrap_or("it ended without answering");
                    return Err(format!("{program}: {why}"));
                }
            },
            _ = &mut deadline => {
                process.kill(true);
                return Err(format!("{program} didn't answer in {} s.", within.as_secs()));
            }
        }
    }
}

/// The program's version, as `--version` prints it first.
pub(super) async fn version(program: &str, env: &[(String, String)]) -> Result<String, String> {
    let args = ["--version".to_string()];
    let out = output(program, &args, env, "", Duration::from_secs(30)).await?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "{program} --version failed: {}",
            err.lines().last().unwrap_or("no output")
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string())
}

/// The error of a run that ended without saying how: stopped, silent for
/// too long, or ended, with what its stderr said last. `partial`: some of
/// the reply was shown.
pub(super) fn ended_error(
    kind: ConnectorKind,
    process: &Process,
    stop: Stop,
    code: Option<i32>,
    stderr: &str,
    partial: bool,
) -> ChatError {
    if stop == Stop::Stopped {
        return ChatError::stopped();
    }
    let mut error = if stop == Stop::Stalled {
        let silent = if process.got_line {
            IDLE
        } else {
            process.first_line_within
        };
        let mut e = ChatError::new(
            ErrorKind::Stalled,
            format!(
                "{} printed nothing for {} s.",
                kind.title(),
                silent.as_secs()
            ),
        );
        e.retryable = false;
        e
    } else if code == Some(EXIT_TERMINATED) {
        ChatError::stopped()
    } else {
        let what = match (stderr.lines().last(), code) {
            (Some(line), _) => line.to_string(),
            (None, Some(code)) => {
                format!("the program ended with exit code {code} before it answered")
            }
            (None, None) => "the program ended before it answered".to_string(),
        };
        harness_error(kind, what, Some(stderr.to_string()))
    };
    error.partial = partial;
    if error.details.is_none() && !stderr.is_empty() {
        error = error.with_details(stderr);
    }
    error
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A program not on the PATH is looked for in the user's own `bin`
    /// first, where the native installers put it, then in the system's.
    #[test]
    fn a_program_is_looked_for_in_the_users_bin_before_the_systems() {
        let places = unix_places("codex");
        let system = [
            "/usr/local/bin/codex",
            "/usr/bin/codex",
            "/opt/rovik/bin/codex",
            "/opt/homebrew/bin/codex",
        ];
        assert_eq!(
            places[places.len() - system.len()..],
            system.map(PathBuf::from)
        );
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(places[0], PathBuf::from(home).join(".local/bin/codex"));
        }
    }

    /// On a team server a connector's program runs in the user's private
    /// network, with its browser for the chat.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_harness_runs_in_the_network_it_is_given_with_the_chats_browser() {
        let Some((_dir, network)) =
            crate::overlay::confine::network::for_test(Path::new("/bin/echo"), &[], None)
        else {
            return;
        };
        crate::rt::block_on(async {
            let script = "readlink /proc/self/ns/net; echo \"chat=$ROVIK_CHAT\"; xdg-open http://localhost:9/";
            let args = ["-c".to_string(), script.to_string()];
            let mut process = Process::spawn(
                "sh",
                &args,
                &[],
                None,
                Some((&network, "chat-9")),
                String::new(),
                Duration::from_secs(30),
                Vec::new(),
            )
            .unwrap();
            let mut lines = Vec::new();
            while let Some(line) = process.lines.recv().await {
                lines.push(line);
            }
            let ours = std::fs::read_link("/proc/self/ns/net").unwrap();
            assert_ne!(lines[0], ours.to_string_lossy(), "{lines:?}");
            assert_eq!(lines[1], "chat=chat-9");
            assert_eq!(lines[2], "open http://localhost:9/");
            let (code, _) = process.finish().await;
            assert_eq!(code, Some(0));
        });
    }

    /// A run that speaks a protocol: what it's sent reaches it as it runs,
    /// and its input closes when the run finishes, which ends it.
    #[test]
    fn a_programs_input_stays_open_for_a_conversation_and_closes_with_the_run() {
        crate::rt::block_on(async {
            let script = "while read line; do echo \"got $line\"; done; echo bye";
            let args = ["-c".to_string(), script.to_string()];
            let mut process = Process::spawn_with_input(
                "sh",
                &args,
                &[],
                None,
                None,
                Duration::from_secs(30),
                Vec::new(),
            )
            .unwrap();
            let input = process.input().unwrap();
            input.send("one\n".into()).unwrap();
            assert_eq!(process.next_line().await.as_deref(), Some("got one"));
            input.send("two\n".into()).unwrap();
            assert_eq!(process.next_line().await.as_deref(), Some("got two"));
            drop(input);
            // The run's own sender goes as it finishes: the program reads
            // the end of its input and ends.
            let (code, _) = process.finish().await;
            assert_eq!(code, Some(0));
            assert_eq!(process.next_line().await.as_deref(), Some("bye"));
            assert_eq!(process.next_line().await, None);
        });
    }

    /// A parser that ends its turn on a line `end`, and is waiting on Rovik
    /// all along when told to.
    struct Ends {
        waits: bool,
        finished: bool,
    }

    impl Lines for Ends {
        fn on_line(&mut self, line: &str, _emit: &mut dyn FnMut(ChatStreamEvent)) {
            self.finished |= line == "end";
        }

        fn finished(&self) -> bool {
            self.finished
        }

        fn waiting(&self) -> bool {
            self.waits
        }
    }

    /// Pumps `script`'s run with `parser`, its first line due in 300 ms.
    async fn pump_script(script: &str, parser: &mut Ends) -> Stop {
        let args = ["-c".to_string(), script.to_string()];
        let mut process = Process::spawn_with_input(
            "sh",
            &args,
            &[],
            None,
            None,
            Duration::from_millis(300),
            Vec::new(),
        )
        .unwrap();
        let (_stop, mut cancel) = watch::channel(false);
        pump(
            &mut process,
            parser,
            None,
            &mut |_| {},
            &mut cancel,
            &mut |_| {},
        )
        .await
    }

    /// A program that serves a protocol stays up after its turn: the turn
    /// ends when its parser says so. One waiting on Rovik is silent, not
    /// stalled; one that isn't, is.
    #[test]
    fn a_turn_ends_when_its_parser_says_and_waiting_on_rovik_is_not_stalling() {
        crate::rt::block_on(async {
            let started = std::time::Instant::now();
            let mut parser = Ends {
                waits: false,
                finished: false,
            };
            let stop = pump_script("echo end; sleep 30", &mut parser).await;
            assert_eq!(stop, Stop::Ended);
            assert!(started.elapsed() < Duration::from_secs(10));

            let late = "sleep 1; echo end; sleep 30";
            let mut waiting = Ends {
                waits: true,
                finished: false,
            };
            assert_eq!(pump_script(late, &mut waiting).await, Stop::Ended);
            let mut silent = Ends {
                waits: false,
                finished: false,
            };
            assert_eq!(pump_script(late, &mut silent).await, Stop::Stalled);
        });
    }

    #[test]
    fn a_one_off_run_gets_its_input_and_a_time_limit() {
        crate::rt::block_on(async {
            let cat = ["-c".to_string(), "cat".to_string()];
            let out = output("sh", &cat, &[], "pong", Duration::from_secs(10))
                .await
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout), "pong");

            let slow = ["-c".to_string(), "sleep 5".to_string()];
            let err = output("sh", &slow, &[], "", Duration::from_millis(200))
                .await
                .unwrap_err();
            assert_eq!(err, "`sh -c sleep 5` didn't answer in 0 s.");

            let err = output("/nowhere/x", &[], &[], "", Duration::from_secs(1))
                .await
                .unwrap_err();
            assert!(err.starts_with("Couldn't run /nowhere/x"), "{err}");
        });
    }

    #[test]
    fn a_program_is_its_path_its_name_on_the_path_or_a_known_place() {
        let dir = crate::testing::temp_dir();
        let placed = dir.0.join("tool");
        std::fs::write(&placed, "").unwrap();
        let path = placed.to_string_lossy().into_owned();
        assert_eq!(
            resolve_program(&path, "tool", &[], "Install it").unwrap(),
            path
        );
        assert_eq!(
            resolve_program("sh", "tool", &[], "Install it").unwrap(),
            "sh"
        );
        let places = [dir.0.join("missing"), placed.clone()];
        assert_eq!(
            resolve_program(" ", "rovik-no-such-tool", &places, "Install it").unwrap(),
            path
        );
        let err = resolve_program("", "rovik-no-such-tool", &[], "Install it").unwrap_err();
        assert!(
            err.starts_with(
                "rovik-no-such-tool isn't installed, or isn't on the PATH. Install it, or"
            ),
            "{err}"
        );
    }
}

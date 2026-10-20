//! Running commands on a host, and reaching its daemon, whatever carries
//! them: Rovik's own SSH (`Ssh`) to a server, or a program here that runs
//! them there ([`ProcessShell`], [`ProcessConnector`]): `wsl.exe` for a
//! WSL distribution on this computer (`wsl`). The deploy, the connection
//! test and the hosts page go through [`HostShell`] and `Connector`, so a
//! host's kind is decided once, where its settings are read.

use crate::connector::{Connecting, Connector, Refusal, Stream};
use crate::deploy::{self, DeployError, Layout};
use crate::release::Releases;
use crate::ssh::{Output, Ssh, EXEC_WAIT};
use std::ffi::OsString;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// A command's output on its way.
pub type Exec<'a> = Pin<Box<dyn Future<Output = Result<Output, String>> + Send + 'a>>;

/// Runs commands on a host as the user, through `sh -c`: what a deploy, an
/// upgrade, a removal, a setup script and the connection test are made of.
pub trait HostShell: Send + Sync {
    /// `command` run by `sh -c` there, `input` on its stdin and then the
    /// end of it, its output collected; given up on after `wait`.
    fn run<'a>(&'a self, command: &'a str, input: &'a [u8], wait: Duration) -> Exec<'a>;
}

impl<'s> dyn HostShell + 's {
    /// `command` there, with nothing on stdin.
    pub async fn exec(&self, command: &str) -> Result<Output, String> {
        self.run(command, &[], EXEC_WAIT).await
    }

    /// `command` there, with `input` on stdin.
    pub async fn exec_with_input(&self, command: &str, input: &[u8]) -> Result<Output, String> {
        self.run(command, input, EXEC_WAIT).await
    }
}

impl HostShell for Ssh {
    fn run<'a>(&'a self, command: &'a str, input: &'a [u8], wait: Duration) -> Exec<'a> {
        Box::pin(async move {
            self.exec_within(command, input, wait)
                .await
                .map_err(|e| e.to_string())
        })
    }
}

/// A host reached through a program here, which runs whatever follows its
/// `args` there: `wsl.exe -d <distro> --exec` for a WSL distribution, `env`
/// for this machine itself (the tests). The commands it runs for
/// [`HostShell`] go through `sh -c` there, as SSH's do.
#[derive(Clone, Debug)]
pub struct ProcessShell {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// What to call the host in an error ("WSL: Ubuntu").
    pub name: String,
}

impl ProcessShell {
    /// The program with its `args`, then `argv`, started as every program
    /// the app starts (no console window on Windows).
    pub fn command(&self, argv: &[&str]) -> Command {
        let mut command = rovik_core::process::command(&self.program);
        rovik_core::process::no_window(&mut command);
        command.args(&self.args).args(argv);
        Command::from(command)
    }

    /// What's said when the program can't be started.
    fn not_started(&self, e: io::Error) -> String {
        format!(
            "{}: {} couldn't start: {e}",
            self.name,
            self.program.display()
        )
    }
}

impl HostShell for ProcessShell {
    fn run<'a>(&'a self, command: &'a str, input: &'a [u8], wait: Duration) -> Exec<'a> {
        Box::pin(async move {
            let mut child = self
                .command(&["sh", "-c", command])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| self.not_started(e))?;
            let mut stdin = child.stdin.take();
            // The input goes in while the output comes out: a command that
            // writes before it reads mustn't block on a full pipe.
            let feed = async {
                if let Some(mut stdin) = stdin.take() {
                    stdin.write_all(input).await?;
                    stdin.shutdown().await?;
                }
                Ok::<(), io::Error>(())
            };
            let done = async {
                let (fed, out) = tokio::join!(feed, child.wait_with_output());
                fed?;
                out
            };
            match tokio::time::timeout(wait, done).await {
                Ok(Ok(out)) => Ok(Output {
                    status: out.status.code().and_then(|c| u32::try_from(c).ok()),
                    stdout: out.stdout,
                    stderr: out.stderr,
                }),
                Ok(Err(e)) => Err(format!("{}: `{command}`: {e}", self.name)),
                Err(_) => Err(format!(
                    "{}: `{command}` didn't finish in {} s.",
                    self.name,
                    wait.as_secs()
                )),
            }
        })
    }
}

/// How long the daemon may take to start before a stream is opened to it.
const START_WAIT: Duration = Duration::from_secs(45);
/// How long a start holds: a stream opened sooner after one trusts it.
const STARTED_FOR: Duration = Duration::from_secs(30);

/// A host's daemon through a program here (D5, as SSH's `attach`): each
/// stream is the program run with `attach` (`rovik-agentd attach
/// --stdio`), its stdin and stdout being the stream. Where the host has no
/// systemd to keep the daemon up (a WSL distribution starts fresh after a
/// reboot or a `wsl --shutdown`), `ensuring`'s script starts it when it
/// isn't running, before a stream, now and then. Rovik's install there is
/// checked once (`with_install_check`, D5).
pub struct ProcessConnector {
    shell: ProcessShell,
    attach: Vec<String>,
    ensure: Option<String>,
    install: Option<InstallCheck>,
    state: tokio::sync::Mutex<State>,
}

struct InstallCheck {
    known: Vec<String>,
    releases: Option<Releases>,
    user: String,
}

#[derive(Default)]
struct State {
    /// When the daemon was last made sure of.
    ensured: Option<Instant>,
    checked: bool,
}

impl ProcessConnector {
    /// The daemon reached through `shell`'s program, each stream running
    /// `attach` there (the program and its arguments).
    pub fn new(shell: ProcessShell, attach: Vec<String>) -> ProcessConnector {
        ProcessConnector {
            shell,
            attach,
            ensure: None,
            install: None,
            state: tokio::sync::Mutex::new(State::default()),
        }
    }

    /// With `script` run there (through `sh -c`) before a stream, to start
    /// the daemon when it isn't running and wait for its socket.
    pub fn ensuring(mut self, script: String) -> ProcessConnector {
        self.ensure = Some(script);
        self
    }

    /// Checks Rovik's install there once (D5, H05), as SSH does on each new
    /// connection: the binary must be a build `known` names by its SHA-256,
    /// or one its version's release lists in `releases`; `user` names the
    /// unit where there's systemd. A failed check refuses the connection.
    pub fn with_install_check(
        mut self,
        known: Vec<String>,
        releases: Option<Releases>,
        user: String,
    ) -> ProcessConnector {
        self.install = Some(InstallCheck {
            known,
            releases,
            user,
        });
        self
    }

    /// The daemon running and checked, before a stream.
    async fn ready(&self) -> io::Result<()> {
        let mut state = self.state.lock().await;
        let fresh = state.ensured.is_some_and(|at| at.elapsed() < STARTED_FOR);
        if let (Some(script), false) = (&self.ensure, fresh) {
            self.shell
                .run(script, &[], START_WAIT)
                .await
                .map_err(io::Error::other)?;
            state.ensured = Some(Instant::now());
        }
        if let (Some(check), false) = (&self.install, state.checked) {
            let layout = Layout::default();
            let shell: &dyn HostShell = &self.shell;
            let checked = deploy::check_install(
                shell,
                &check.known,
                check.releases.as_ref(),
                &check.user,
                &layout,
            );
            checked.await.map_err(|e| match e {
                // The check couldn't be made now: no verdict on the install.
                DeployError::Failed(why) => io::Error::other(why),
                e => Refusal {
                    why: e.to_string(),
                    may_pass: true,
                }
                .into_error(),
            })?;
            state.checked = true;
        }
        Ok(())
    }
}

impl Connector for ProcessConnector {
    fn connect(&self) -> Connecting<'_> {
        Box::pin(async move {
            self.ready().await?;
            let argv: Vec<&str> = self.attach.iter().map(String::as_str).collect();
            let mut child = self
                .shell
                .command(&argv)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| io::Error::other(self.shell.not_started(e)))?;
            let (stdin, stdout) = (child.stdin.take(), child.stdout.take());
            let (Some(stdin), Some(stdout)) = (stdin, stdout) else {
                return Err(io::Error::other("the program's pipes weren't made"));
            };
            Ok(Box::new(ChildStream {
                _child: child,
                io: tokio::io::join(stdout, stdin),
            }) as Stream)
        })
    }
}

/// A program's stdout and stdin as one stream; the program ends with it.
struct ChildStream {
    _child: Child,
    io: tokio::io::Join<ChildStdout, ChildStdin>,
}

impl AsyncRead for ChildStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for ChildStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    /// This machine itself: `env` runs what follows it.
    fn here() -> ProcessShell {
        ProcessShell {
            program: "env".into(),
            args: Vec::new(),
            name: "this machine".into(),
        }
    }

    #[tokio::test]
    async fn a_process_shell_runs_the_command_with_its_input() {
        let shell: &dyn HostShell = &here();
        let out = shell
            .exec_with_input("cat; echo more >&2; exit 3", b"hi\n")
            .await
            .unwrap();
        assert_eq!(out.status, Some(3));
        assert_eq!(out.stdout, b"hi\n");
        assert_eq!(out.stderr, b"more\n");
    }

    #[tokio::test]
    async fn a_process_shell_gives_up_in_time() {
        let shell = here();
        let late = shell.run("sleep 30", &[], Duration::from_millis(300)).await;
        assert!(late.unwrap_err().contains("didn't finish"));
    }

    /// A stream is the attach program's stdin and stdout (`cat` here, which
    /// echoes), opened once the ensuring script ran.
    #[tokio::test]
    async fn a_process_connector_bridges_the_programs_stdio() {
        let marker = std::env::temp_dir().join(format!("ensured-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let connector = ProcessConnector::new(here(), vec!["cat".into()]).ensuring(format!(
            "touch {}",
            crate::ssh::quote(&marker.to_string_lossy())
        ));
        let mut stream = connector.connect().await.unwrap();
        stream.write_all(b"ping").await.unwrap();
        let mut echoed = [0u8; 4];
        stream.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"ping");
        assert!(marker.exists(), "the ensuring script ran first");
        let _ = std::fs::remove_file(&marker);
    }

    #[tokio::test]
    async fn a_program_that_cannot_start_is_a_link_that_did_not_come_up() {
        let shell = ProcessShell {
            program: "/nonexistent/rovik-test-program".into(),
            args: Vec::new(),
            name: "nowhere".into(),
        };
        let connector = ProcessConnector::new(shell, vec!["cat".into()]);
        let e = connector
            .connect()
            .await
            .err()
            .expect("a program that isn't there can't be started");
        assert!(Refusal::of(&e).is_none(), "{e}");
        assert!(e.to_string().contains("couldn't start"), "{e}");
    }
}

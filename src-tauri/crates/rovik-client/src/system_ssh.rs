//! "Use my system ssh" (H03): the user's own `ssh` forwards a socket here
//! to the daemon's there, so jump hosts, FIDO keys, certificates and the
//! rest of their SSH configuration apply as they do in a terminal. It runs
//! under the app, started with the first stream and again once it has
//! exited, and stopped when the connector goes. The daemon's socket there
//! comes from `$HOME`, asked once through the same ssh. The terminal
//! panel's remote terminals on such a host run it too, each in a terminal
//! here (`terminal_args`).
//!
//! The forward listens on a socket in a folder of the app's on Unix, and
//! on a port of 127.0.0.1 elsewhere, which only this user's processes
//! reach on a desktop.

use crate::connector::{Connecting, Connector, Stream};
use crate::ssh::SOCKET;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

/// How long the forward may take to be ready: a key to touch, a jump host
/// to pass.
const READY_WAIT: Duration = Duration::from_secs(60);

/// The user's ssh, started as every program the app starts is
/// (`rovik_core::process::command`).
fn ssh_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    Command::from(rovik_core::process::command(program))
}

/// The user's own ssh, and where it goes.
#[derive(Clone, Debug)]
pub struct SystemSsh {
    /// What the user types after `ssh`: `user@host`, or a name from their
    /// SSH configuration.
    pub destination: String,
    pub port: Option<u16>,
    /// The program: `ssh` on the PATH unless told otherwise.
    pub program: PathBuf,
}

/// Where the forward listens here.
#[derive(Clone, Debug, PartialEq)]
pub enum Local {
    #[cfg(unix)]
    Socket(PathBuf),
    Port(u16),
}

impl SystemSsh {
    /// What it's run with before the destination: the port, and options
    /// that keep a dead link from looking alive.
    fn options(&self) -> Vec<OsString> {
        let mut options: Vec<OsString> = Vec::new();
        if let Some(port) = self.port {
            options.extend(["-p".into(), port.to_string().into()]);
        }
        for option in ["ServerAliveInterval=15", "ServerAliveCountMax=3"] {
            options.extend(["-o".into(), option.into()]);
        }
        options
    }

    /// The forward: `ssh -N -L <local>:<remote> <destination>`, refusing to
    /// start when the forward can't be set up, and replacing a socket an
    /// earlier run left.
    pub fn forward_args(&self, local: &Local, remote: &str) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec!["-N".into()];
        args.extend(self.options());
        for option in ["ExitOnForwardFailure=yes", "StreamLocalBindUnlink=yes"] {
            args.extend(["-o".into(), option.into()]);
        }
        let listen = match local {
            #[cfg(unix)]
            Local::Socket(path) => path.to_string_lossy().into_owned(),
            Local::Port(port) => format!("127.0.0.1:{port}"),
        };
        args.extend(["-L".into(), format!("{listen}:{remote}").into()]);
        args.push(self.destination.clone().into());
        args
    }

    /// `ssh <destination> <command>`.
    fn command_args(&self, command: &str) -> Vec<OsString> {
        let mut args = self.options();
        args.push(self.destination.clone().into());
        args.push(command.into());
        args
    }

    /// A terminal's: `ssh -t <destination>`, the user's login shell there
    /// on a pseudo-terminal, run in one here.
    pub fn terminal_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec!["-t".into()];
        args.extend(self.options());
        args.push(self.destination.clone().into());
        args
    }
}

/// The daemon through the user's own ssh.
pub struct SystemSshConnector {
    ssh: SystemSsh,
    local: Local,
    /// The daemon's socket there, asked for once.
    remote: tokio::sync::Mutex<Option<String>>,
    forward: tokio::sync::Mutex<Option<Child>>,
}

impl SystemSshConnector {
    pub fn new(ssh: SystemSsh, local: Local) -> SystemSshConnector {
        SystemSshConnector {
            ssh,
            local,
            remote: tokio::sync::Mutex::new(None),
            forward: tokio::sync::Mutex::new(None),
        }
    }

    /// The daemon's socket at `path` there, an absolute path.
    pub fn with_remote(self, path: impl Into<String>) -> SystemSshConnector {
        *self.remote.try_lock().expect("a new connector") = Some(path.into());
        self
    }

    /// The daemon's socket there: asked for once, from `$HOME`.
    pub async fn remote_socket(&self) -> io::Result<String> {
        let mut remote = self.remote.lock().await;
        if let Some(path) = remote.as_ref() {
            return Ok(path.clone());
        }
        let asked = ssh_command(&self.ssh.program)
            .args(self.ssh.command_args("printf '%s' \"$HOME\""))
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(READY_WAIT, asked)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "ssh didn't answer in time"))??;
        let home = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !output.status.success() || !home.starts_with('/') {
            let said = String::from_utf8_lossy(&output.stderr);
            return Err(io::Error::other(format!(
                "ssh {} didn't say where your home folder is: {}",
                self.ssh.destination,
                said.trim()
            )));
        }
        let path = format!("{}/{SOCKET}", home.trim_end_matches('/'));
        *remote = Some(path.clone());
        Ok(path)
    }

    /// The forward, started if it isn't running, and ready.
    async fn forward(&self) -> io::Result<()> {
        let mut forward = self.forward.lock().await;
        if let Some(child) = forward.as_mut() {
            if child.try_wait()?.is_none() {
                return Ok(());
            }
        }
        let remote = self.remote_socket().await?;
        #[cfg(unix)]
        if let Local::Socket(path) = &self.local {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let mut child = ssh_command(&self.ssh.program)
            .args(self.ssh.forward_args(&self.local, &remote))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let started = Instant::now();
        loop {
            if self.reach().await.is_ok() {
                *forward = Some(child);
                return Ok(());
            }
            if child.try_wait()?.is_some() {
                let mut said = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut said).await;
                }
                return Err(io::Error::other(format!(
                    "ssh {} stopped: {}",
                    self.ssh.destination,
                    said.trim()
                )));
            }
            if started.elapsed() > READY_WAIT {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("ssh {} didn't forward in time", self.ssh.destination),
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// A stream to the forward's end here.
    async fn reach(&self) -> io::Result<Stream> {
        match &self.local {
            #[cfg(unix)]
            Local::Socket(path) => {
                let stream = tokio::net::UnixStream::connect(path).await?;
                Ok(Box::new(stream))
            }
            Local::Port(port) => {
                let stream = tokio::net::TcpStream::connect(("127.0.0.1", *port)).await?;
                Ok(Box::new(stream))
            }
        }
    }
}

impl Connector for SystemSshConnector {
    fn connect(&self) -> Connecting<'_> {
        Box::pin(async move {
            self.forward().await?;
            self.reach().await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh() -> SystemSsh {
        SystemSsh {
            destination: "build-box".into(),
            port: Some(2222),
            program: "ssh".into(),
        }
    }

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn the_forward_runs_the_users_ssh_with_its_configuration() {
        let local = Local::Socket("/run/user/1000/rovik/h1.sock".into());
        let args = strings(ssh().forward_args(&local, "/home/me/.rovik/agentd.sock"));
        assert_eq!(
            args,
            [
                "-N",
                "-p",
                "2222",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "StreamLocalBindUnlink=yes",
                "-L",
                "/run/user/1000/rovik/h1.sock:/home/me/.rovik/agentd.sock",
                "build-box",
            ]
        );
    }

    #[test]
    fn a_port_here_listens_on_loopback_alone() {
        let mut no_port = ssh();
        no_port.port = None;
        let args = strings(no_port.forward_args(&Local::Port(40123), "/h/.rovik/agentd.sock"));
        let at = args.iter().position(|a| a == "-L").unwrap();
        assert_eq!(args[at + 1], "127.0.0.1:40123:/h/.rovik/agentd.sock");
        assert!(!args.contains(&"-p".to_string()));
        assert_eq!(args.last().unwrap(), "build-box");
    }

    #[test]
    fn a_terminal_runs_the_users_ssh_with_a_terminal_there() {
        let args = strings(ssh().terminal_args());
        assert_eq!(
            args,
            [
                "-t",
                "-p",
                "2222",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "build-box",
            ]
        );
    }

    #[test]
    fn home_is_asked_through_the_same_ssh() {
        let args = strings(ssh().command_args("printf '%s' \"$HOME\""));
        assert_eq!(args[..2], ["-p", "2222"]);
        assert_eq!(
            args[args.len() - 2..],
            ["build-box", "printf '%s' \"$HOME\""]
        );
    }
}

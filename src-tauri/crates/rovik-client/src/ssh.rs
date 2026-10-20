//! SSH to a host (H02, D5, D6), with russh: one connection per host, and
//! over it a `direct-streamlocal` channel to the daemon's socket for each
//! stream a `Client` asks for (the WebSocket, each `POST /call`, each
//! pack), and exec channels for setup (H04, H05). Keepalives every 15 s
//! find a dead link; the next stream asked for connects again. The
//! terminal panel's remote terminals each have a connection of their own,
//! with the user's login shell on a pseudo-terminal there (`Ssh::shell`).
//!
//! The socket is `~/.rovik/agentd.sock` there, its path from `$HOME`, asked
//! once (H03). A host that refuses streamlocal forwarding gets each stream
//! through `rovik-agentd attach --stdio` on an exec channel instead, a byte
//! bridge to the socket. The user's own ssh is the other way in
//! (`system_ssh`).
//!
//! The host key is checked before any credential goes. The first time
//! there's no key to check it against, and the connection fails with
//! `UnknownHostKey`, the key and its SHA-256 fingerprint in it: the user
//! compares the fingerprint, and the key is pinned (`Host::host_key`). A
//! different key later is refused, whatever the settings say.
//!
//! Logging in takes a key file and its passphrase, the SSH agent's keys
//! (`SSH_AUTH_SOCK`; on Windows the OpenSSH agent's pipe, then Pageant), a
//! password, or the server's own questions (keyboard-interactive).

use crate::connector::{Connecting, Connector, Refusal, Stream};
use rovik_protocol::Secret;
use russh::client::{self, AuthResult, Handle, Handler, KeyboardInteractiveAuthResponse};
use russh::keys::agent::client::AgentClient;
use russh::keys::agent::AgentIdentity;
use russh::keys::{Algorithm, HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh::{ChannelMsg, ChannelReadHalf};
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long an idle link waits before it's asked whether it's alive; three
/// unanswered, and it's dropped.
pub const KEEPALIVE: Duration = Duration::from_secs(15);

/// How long the handshake may take, once connected (the connection's own
/// timeout is Settings → Network's).
const CONNECT_WAIT: Duration = Duration::from_secs(20);

/// How long a command on the host may take before it's given up on
/// (`exec`): the setup commands take seconds, a daemon's stop at most
/// 100 s.
pub const EXEC_WAIT: Duration = Duration::from_secs(15 * 60);

/// The most of a command's output kept, per stream: a host that never
/// stops printing can't fill the app's memory.
const OUTPUT_CAP: usize = 16 << 20;

/// The command whose stdin and stdout reach the daemon's socket, where the
/// host refuses streamlocal forwarding: the installed binary (5.3).
pub const ATTACH: &str = "/opt/rovik/bin/rovik-agentd attach --stdio";

/// The daemon's socket, from the user's home folder there (D5).
pub const SOCKET: &str = ".rovik/agentd.sock";

/// Where, and as whom.
#[derive(Clone)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    /// The host key pinned on first use, as OpenSSH writes it.
    pub host_key: Option<String>,
}

impl SshTarget {
    /// The pinned host key's fingerprint, when one is pinned.
    pub fn pinned_fingerprint(&self) -> Result<Option<String>, SshError> {
        Ok(self.pinned_key()?.as_ref().map(fingerprint))
    }

    fn pinned_key(&self) -> Result<Option<PublicKey>, SshError> {
        let Some(key) = &self.host_key else {
            return Ok(None);
        };
        PublicKey::from_openssh(key)
            .map(Some)
            .map_err(|e| SshError::Key(format!("the pinned host key can't be read: {e}")))
    }
}

/// How to log in.
#[derive(Clone)]
pub enum SshAuth {
    /// A private key file, and its passphrase if it has one.
    Key {
        path: PathBuf,
        passphrase: Option<Secret>,
    },
    /// The SSH agent's keys: the agent at `socket`, else the user's.
    Agent {
        socket: Option<PathBuf>,
    },
    Password(Secret),
    /// The server's questions, each round answered by `Answerer`.
    KeyboardInteractive(Answerer),
}

/// A round of the server's questions, as keyboard-interactive asks them.
#[derive(Clone, Debug, PartialEq)]
pub struct Questions {
    pub name: String,
    pub instructions: String,
    /// Each prompt, and whether what's typed shows.
    pub prompts: Vec<(String, bool)>,
}

/// Answers a round of questions, one answer a prompt, or gives up (None).
pub type Answerer = Arc<
    dyn Fn(Questions) -> Pin<Box<dyn Future<Output = Option<Vec<Secret>>> + Send>> + Send + Sync,
>;

#[derive(Clone, Debug, PartialEq)]
pub enum SshError {
    /// The host couldn't be reached: its name, the network, or the
    /// handshake, before its key was checked.
    Unreachable(String),
    /// No key is pinned for the host yet: the user is to compare this
    /// fingerprint with the host's own before the key is pinned.
    UnknownHostKey { key: String, fingerprint: String },
    /// The host's key isn't the one pinned: refused, as a man in the
    /// middle would show another.
    HostKeyChanged { pinned: String, found: String },
    /// The host didn't take the credentials.
    Denied(String),
    /// The key file or the agent failed: not there, not up, not answering.
    Key(String),
    /// The key file is there and can't be used: not a key, or not one its
    /// passphrase (or none) opens. For the user, as the host's refusal is.
    KeyUnusable(String),
    /// The connection dropped while logging in, or a channel or a
    /// command failed.
    Failed(String),
    /// Rovik on the host isn't as installed: the check each connection
    /// makes refused it (D5, H05).
    Install(String),
}

impl fmt::Display for SshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SshError::Unreachable(why) => write!(f, "The host couldn't be reached: {why}"),
            SshError::UnknownHostKey { fingerprint, .. } => write!(
                f,
                "This host's key isn't known yet. Check that its fingerprint is {fingerprint}."
            ),
            SshError::HostKeyChanged { pinned, found } => write!(
                f,
                "This host's key changed: it was {pinned}, it's {found} now. Someone may be \
                 between you and the host; if the host was reinstalled, remove it and add it \
                 again."
            ),
            SshError::Denied(why) => write!(f, "The host didn't let you in: {why}"),
            SshError::Key(why) => write!(f, "The SSH key: {why}"),
            SshError::KeyUnusable(why) => write!(f, "The SSH key can't be used: {why}"),
            SshError::Failed(why) | SshError::Install(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for SshError {}

/// A host key's SHA-256 fingerprint, as OpenSSH shows it.
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// Checks the host's key against the one pinned, and keeps what it saw for
/// the error a refusal becomes.
struct Checker {
    pinned: Option<PublicKey>,
    seen: Arc<Mutex<Option<PublicKey>>>,
}

impl Handler for Checker {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        offered: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // Host certificates aren't pinned in v1: the user's own ssh takes
        // them (H03).
        let PublicKeyOrCertificate::PublicKey { key, .. } = offered else {
            return Ok(false);
        };
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(key.clone());
        let pinned = self.pinned.as_ref();
        Ok(pinned.is_some_and(|pinned| pinned.key_data() == key.key_data()))
    }
}

/// A connection to a host, logged in.
pub struct Ssh {
    handle: Handle<Checker>,
    /// The host refused a `direct-streamlocal` channel on this connection:
    /// streams go through `attach` from then on.
    streamlocal_refused: AtomicBool,
}

/// What a command printed, and how it ended.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Output {
    pub status: Option<u32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Ssh {
    /// Connects to the host, checks its key and logs in.
    pub async fn connect(target: &SshTarget) -> Result<Ssh, SshError> {
        let pinned = target.pinned_key()?;
        let seen = Arc::new(Mutex::new(None));
        let checker = Checker {
            pinned: pinned.clone(),
            seen: Arc::clone(&seen),
        };
        let mut preferred = russh::Preferred::DEFAULT.clone();
        if let Some(pinned) = &pinned {
            // The pinned key's kind first, as ssh does with known_hosts: a
            // host that has keys of several kinds shows the one it's known
            // by, not one it gained since, which would read as a change.
            preferred.key = preferring(&pinned.algorithm(), &preferred.key).into();
        }
        let config = Arc::new(client::Config {
            preferred,
            keepalive_interval: Some(KEEPALIVE),
            keepalive_max: 3,
            inactivity_timeout: None,
            nodelay: true,
            ..Default::default()
        });
        // Directly, or through the proxy (Settings → Network), within its
        // timeout; then the handshake.
        let stream = rovik_core::net::open_cloud_host(&target.host, target.port)
            .await
            .map_err(SshError::Unreachable)?;
        let connected = tokio::time::timeout(
            CONNECT_WAIT,
            client::connect_stream(config, stream, checker),
        );
        let mut handle = match connected.await {
            Ok(Ok(handle)) => handle,
            Ok(Err(russh::Error::UnknownKey)) => {
                let seen = seen.lock().unwrap_or_else(|e| e.into_inner()).take();
                return Err(key_refused(seen, pinned.as_ref()));
            }
            Ok(Err(e)) => return Err(SshError::Unreachable(e.to_string())),
            Err(_) => {
                return Err(SshError::Unreachable(format!(
                    "{} didn't answer in time.",
                    target.host
                )))
            }
        };
        log_in(&mut handle, target).await?;
        Ok(Ssh {
            handle,
            streamlocal_refused: AtomicBool::new(false),
        })
    }

    /// Whether the connection dropped.
    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    /// A stream to the Unix socket at `path` on the host.
    pub async fn stream_to(&self, path: &str) -> Result<Stream, SshError> {
        let channel = self
            .handle
            .channel_open_direct_streamlocal(path)
            .await
            .map_err(|e| SshError::Failed(format!("{path} on the host: {e}")))?;
        Ok(Box::new(channel.into_stream()))
    }

    /// A stream to the daemon: a channel to its socket at `socket`, or
    /// where the host refuses those channels, `attach`'s stdin and stdout.
    /// A socket the host can't reach goes through `attach` too, which finds
    /// the daemon where it is.
    pub async fn stream_to_daemon(&self, socket: &str, attach: &str) -> Result<Stream, SshError> {
        if !self.streamlocal_refused.load(Ordering::Relaxed) {
            match self.handle.channel_open_direct_streamlocal(socket).await {
                Ok(channel) => return Ok(Box::new(channel.into_stream())),
                Err(russh::Error::ChannelOpenFailure(
                    russh::ChannelOpenFailure::AdministrativelyProhibited,
                )) => self.streamlocal_refused.store(true, Ordering::Relaxed),
                Err(russh::Error::ChannelOpenFailure(_)) => {}
                Err(e) => return Err(SshError::Failed(format!("{socket} on the host: {e}"))),
            }
        }
        self.exec_stream(attach).await
    }

    /// `command` on the host, its stdin and stdout as a stream.
    pub async fn exec_stream(&self, command: &str) -> Result<Stream, SshError> {
        let failed = |e: russh::Error| SshError::Failed(format!("`{command}`: {e}"));
        let channel = self.handle.channel_open_session().await.map_err(failed)?;
        channel.exec(false, command).await.map_err(failed)?;
        Ok(Box::new(channel.into_stream()))
    }

    /// The user's home folder on the host.
    pub async fn home(&self) -> Result<String, SshError> {
        let output = self.exec("printf '%s' \"$HOME\"").await?;
        let home = String::from_utf8_lossy(&output.stdout).trim().to_string();
        match output.status == Some(0) && home.starts_with('/') {
            true => Ok(home),
            false => Err(SshError::Failed(
                "The host didn't say where your home folder is.".into(),
            )),
        }
    }

    /// Runs `command` on the host and collects its output.
    pub async fn exec(&self, command: &str) -> Result<Output, SshError> {
        self.exec_with_input(command, &[]).await
    }

    /// Runs `command` on the host with `input` on its stdin, then the end
    /// of it, and collects its output; given up on after `EXEC_WAIT`.
    pub async fn exec_with_input(&self, command: &str, input: &[u8]) -> Result<Output, SshError> {
        self.exec_within(command, input, EXEC_WAIT).await
    }

    /// `exec_with_input`, given up on after `wait`: a host that hangs
    /// mustn't hang the app.
    pub async fn exec_within(
        &self,
        command: &str,
        input: &[u8],
        wait: Duration,
    ) -> Result<Output, SshError> {
        match tokio::time::timeout(wait, self.exec_now(command, input)).await {
            Ok(result) => result,
            Err(_) => Err(SshError::Failed(format!(
                "`{command}` didn't finish on the host in {} s.",
                wait.as_secs()
            ))),
        }
    }

    async fn exec_now(&self, command: &str, input: &[u8]) -> Result<Output, SshError> {
        let failed = |e: russh::Error| SshError::Failed(format!("`{command}`: {e}"));
        let mut channel = self.handle.channel_open_session().await.map_err(failed)?;
        // Through sh: the commands are written for it, and the login shell
        // the host would run them in may be another (fish, say).
        let through_sh = format!("sh -c {}", quote(command));
        channel
            .exec(true, through_sh.as_str())
            .await
            .map_err(failed)?;
        if !input.is_empty() {
            channel.data(input).await.map_err(failed)?;
        }
        channel.eof().await.map_err(failed)?;
        let mut output = Output::default();
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => keep(&mut output.stdout, &data),
                ChannelMsg::ExtendedData { data, ext: 1 } => keep(&mut output.stderr, &data),
                ChannelMsg::ExitStatus { exit_status } => output.status = Some(exit_status),
                ChannelMsg::Failure => {
                    return Err(SshError::Failed(format!(
                        "the host refused to run `{command}`"
                    )))
                }
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        Ok(output)
    }

    /// The name of the user's login shell on the host ("bash"), as its
    /// `$SHELL` says; None when it says nothing.
    pub async fn login_shell(&self) -> Option<String> {
        let asked = self.exec_within("printf '%s' \"$SHELL\"", &[], CONNECT_WAIT);
        let output = asked.await.ok()?;
        let path = String::from_utf8_lossy(&output.stdout);
        let name = path.trim().rsplit('/').next()?;
        (output.status == Some(0) && !name.is_empty()).then(|| name.to_string())
    }

    /// The user's login shell on the host, on a pseudo-terminal there of
    /// `cols` columns and `rows` rows that takes `term`'s escapes: what's
    /// typed goes in through the `Shell`, what the shell prints comes out
    /// of the `ShellOutput`. The `Shell` holds the connection: once it
    /// hangs up, the connection closes, unless something else holds it.
    pub async fn shell(
        self: Arc<Self>,
        term: &str,
        cols: u32,
        rows: u32,
    ) -> Result<(Shell, ShellOutput), SshError> {
        let failed = |e: russh::Error| SshError::Failed(format!("A shell on the host: {e}"));
        let channel = self.handle.channel_open_session().await.map_err(failed)?;
        // No reply asked for: a host that gives no terminal still runs the
        // shell, a plain one, as ssh's does.
        channel
            .request_pty(false, term, cols, rows, 0, 0, &[])
            .await
            .map_err(failed)?;
        channel.request_shell(true).await.map_err(failed)?;
        let (mut read, write) = channel.split();
        let early = match tokio::time::timeout(CONNECT_WAIT, started(&mut read)).await {
            Ok(early) => early?,
            Err(_) => {
                return Err(SshError::Failed(
                    "The host didn't start a shell in time.".into(),
                ))
            }
        };
        let (input, mut typed) = tokio::sync::mpsc::unbounded_channel();
        let hang_up = Arc::new(tokio::sync::Notify::new());
        let hung_up = Arc::clone(&hang_up);
        rovik_core::rt::spawn(async move {
            let sending = async {
                while let Some(input) = typed.recv().await {
                    let sent = match input {
                        ShellInput::Typed(data) => write.data_bytes(data).await,
                        ShellInput::Resized { cols, rows } => {
                            write.window_change(cols, rows, 0, 0).await
                        }
                    };
                    if sent.is_err() {
                        break;
                    }
                }
            };
            // A hang-up doesn't wait for room to send a big paste to a
            // shell that doesn't read it.
            tokio::select! {
                () = sending => {}
                () = hung_up.notified() => {}
            }
            // Nothing more goes in; the host ends the shell, and the
            // connection goes with this task.
            drop(typed);
            let _ = tokio::time::timeout(CONNECT_WAIT, write.close()).await;
            drop(self);
        });
        let shell = Shell { input, hang_up };
        let output = ShellOutput {
            read,
            early: early.into(),
            status: None,
            ended: false,
        };
        Ok((shell, output))
    }
}

/// Waits for the host to say whether a shell started; what it printed
/// first, on a host that doesn't wait to say, is kept.
async fn started(read: &mut ChannelReadHalf) -> Result<Vec<Vec<u8>>, SshError> {
    let mut early = Vec::new();
    loop {
        match read.wait().await {
            Some(ChannelMsg::Success) => return Ok(early),
            Some(ChannelMsg::Failure) => {
                return Err(SshError::Failed(
                    "The host refused to start a shell.".into(),
                ))
            }
            Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                early.push(data.to_vec())
            }
            Some(ChannelMsg::Close) | None => {
                return Err(SshError::Failed(
                    "The host closed the shell as it started.".into(),
                ))
            }
            Some(_) => {}
        }
    }
}

/// A shell on the host (`Ssh::shell`): what's typed goes to it, and the
/// terminal's size, in the order they come. Dropped, it hangs up.
pub struct Shell {
    input: tokio::sync::mpsc::UnboundedSender<ShellInput>,
    /// Tells the task that sends them, which holds the connection.
    hang_up: Arc<tokio::sync::Notify>,
}

enum ShellInput {
    Typed(Vec<u8>),
    Resized { cols: u32, rows: u32 },
}

impl Shell {
    /// Sends what was typed; an error once the shell is gone.
    pub fn write(&self, data: &[u8]) -> Result<(), SshError> {
        self.send(ShellInput::Typed(data.to_vec()))
    }

    /// Tells the host the terminal's new size.
    pub fn resize(&self, cols: u32, rows: u32) -> Result<(), SshError> {
        self.send(ShellInput::Resized { cols, rows })
    }

    /// Hangs up: the host ends the shell, and the connection closes.
    pub fn close(&self) {
        // Kept until the task waits for it, if it's busy sending.
        self.hang_up.notify_one();
    }

    fn send(&self, input: ShellInput) -> Result<(), SshError> {
        let gone = |_| SshError::Failed("The shell on the host has ended.".into());
        self.input.send(input).map_err(gone)
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        self.close();
    }
}

/// What a shell on the host does, as it does it.
#[derive(Clone, Debug, PartialEq)]
pub enum ShellEvent {
    /// What it printed, its errors with it, as the terminal shows them.
    Output(Vec<u8>),
    /// It ended: its exit status, when the host said one.
    Exit(Option<u32>),
}

/// What a shell on the host prints, then its end (`Ssh::shell`).
pub struct ShellOutput {
    read: ChannelReadHalf,
    /// What it printed before the host said it started.
    early: VecDeque<Vec<u8>>,
    status: Option<u32>,
    ended: bool,
}

impl ShellOutput {
    /// What the shell printed next; its end once the host closes its
    /// channel, or the connection is gone (a hang-up, a dead link); None
    /// after that.
    pub async fn next(&mut self) -> Option<ShellEvent> {
        if let Some(data) = self.early.pop_front() {
            return Some(ShellEvent::Output(data));
        }
        if self.ended {
            return None;
        }
        while let Some(message) = self.read.wait().await {
            match message {
                ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => {
                    return Some(ShellEvent::Output(data.to_vec()))
                }
                ChannelMsg::ExitStatus { exit_status } => self.status = Some(exit_status),
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        self.ended = true;
        Some(ShellEvent::Exit(self.status))
    }
}

/// Appends `data` to `into`, up to `OUTPUT_CAP`: the start of a long
/// output is what the commands' readers look at.
fn keep(into: &mut Vec<u8>, data: &[u8]) {
    let room = OUTPUT_CAP.saturating_sub(into.len());
    into.extend_from_slice(&data[..data.len().min(room)]);
}

/// `text` in single quotes, for sh.
pub fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// `order`, with the algorithms of `pinned`'s kind first: RSA keys sign
/// with any of their hashes, ECDSA keys are their curve's.
fn preferring(pinned: &Algorithm, order: &[Algorithm]) -> Vec<Algorithm> {
    let same_kind = |a: &Algorithm| match (a, pinned) {
        (Algorithm::Rsa { .. }, Algorithm::Rsa { .. }) => true,
        (Algorithm::Ecdsa { curve }, Algorithm::Ecdsa { curve: pinned }) => curve == pinned,
        (a, pinned) => a == pinned,
    };
    let (first, rest): (Vec<Algorithm>, Vec<Algorithm>) =
        order.iter().cloned().partition(same_kind);
    first.into_iter().chain(rest).collect()
}

/// Why the host's key was refused: not pinned yet, or not the one pinned.
fn key_refused(seen: Option<PublicKey>, pinned: Option<&PublicKey>) -> SshError {
    let Some(seen) = seen else {
        return SshError::Unreachable("the host showed a key of a kind Rovik doesn't pin".into());
    };
    match pinned {
        Some(pinned) => SshError::HostKeyChanged {
            pinned: fingerprint(pinned),
            found: fingerprint(&seen),
        },
        None => SshError::UnknownHostKey {
            key: seen.to_openssh().unwrap_or_default(),
            fingerprint: fingerprint(&seen),
        },
    }
}

/// The connection dropped after the host's key was checked.
fn dropped(e: russh::Error) -> SshError {
    SshError::Failed(format!("The connection dropped while logging in: {e}"))
}

async fn log_in(handle: &mut Handle<Checker>, target: &SshTarget) -> Result<(), SshError> {
    let user = target.user.clone();
    let result = match &target.auth {
        SshAuth::Password(password) => handle
            .authenticate_password(user, password.0.clone())
            .await
            .map_err(dropped)?,
        SshAuth::Key { path, passphrase } => {
            let passphrase = passphrase.as_ref().map(|p| p.0.as_str());
            // A key file that isn't there (a drive not mounted) is tried
            // again soon; one that's there and can't be read with this
            // passphrase waits for the user, as the host's refusal would.
            let key = russh::keys::load_secret_key(path, passphrase).map_err(|e| {
                let why = format!("{}: {e}", path.display());
                match std::fs::metadata(path) {
                    Err(_) => SshError::Key(why),
                    Ok(_) => SshError::KeyUnusable(why),
                }
            })?;
            let hash = handle
                .best_supported_rsa_hash()
                .await
                .map_err(dropped)?
                .flatten();
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
            handle
                .authenticate_publickey(user, key)
                .await
                .map_err(dropped)?
        }
        SshAuth::Agent { socket } => return with_agent(handle, &user, socket.as_ref()).await,
        SshAuth::KeyboardInteractive(answer) => return interactively(handle, &user, answer).await,
    };
    match result {
        AuthResult::Success => Ok(()),
        AuthResult::Failure { .. } => Err(SshError::Denied(format!(
            "{} isn't the way in for {}.",
            match target.auth {
                SshAuth::Password(_) => "That password",
                _ => "That key",
            },
            target.user
        ))),
    }
}

/// Logs in with each of the agent's keys in turn, until one is taken.
async fn with_agent(
    handle: &mut Handle<Checker>,
    user: &str,
    socket: Option<&PathBuf>,
) -> Result<(), SshError> {
    let mut agent = agent(socket).await?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|e| SshError::Key(format!("the SSH agent: {e}")))?;
    let hash = handle
        .best_supported_rsa_hash()
        .await
        .map_err(dropped)?
        .flatten();
    let (mut last_error, mut offered) = (None, false);
    for identity in identities {
        // Certificates are the user's own ssh's (H03).
        let AgentIdentity::PublicKey { key, .. } = identity else {
            continue;
        };
        offered = true;
        // A key the agent won't sign with (a security key not touched in
        // time, a confirmation declined) leaves the others to try.
        let tried = match handle
            .authenticate_publickey_with(user, key, hash, &mut agent)
            .await
        {
            Ok(tried) => tried,
            Err(e) => {
                last_error = Some(format!("{e:?}"));
                continue;
            }
        };
        if tried.success() {
            return Ok(());
        }
    }
    // None went to the host: an agent whose keys aren't added yet, which
    // trying again later may find there.
    if !offered {
        return Err(SshError::Key(
            "the SSH agent holds no key to log in with (its certificates are your system \
             ssh's): add yours to it, with ssh-add."
                .into(),
        ));
    }
    Err(SshError::Denied(match last_error {
        Some(e) => {
            format!("none of the SSH agent's keys is the way in for {user} (the agent said: {e}).")
        }
        None => format!("none of the SSH agent's keys is the way in for {user}."),
    }))
}

type AnyAgent = AgentClient<Box<dyn russh::keys::agent::client::AgentStream + Send + Unpin>>;

#[cfg(unix)]
async fn agent(socket: Option<&PathBuf>) -> Result<AnyAgent, SshError> {
    let connected = match socket {
        Some(socket) => AgentClient::connect_uds(socket).await,
        None => AgentClient::connect_env().await,
    };
    connected
        .map(AgentClient::dynamic)
        .map_err(|e| SshError::Key(format!("no SSH agent answered: {e}")))
}

#[cfg(windows)]
async fn agent(socket: Option<&PathBuf>) -> Result<AnyAgent, SshError> {
    let pipe = socket
        .map(|s| s.as_os_str().to_owned())
        .unwrap_or_else(|| r"\\.\pipe\openssh-ssh-agent".into());
    if let Ok(agent) = AgentClient::connect_named_pipe(&pipe).await {
        return Ok(agent.dynamic());
    }
    AgentClient::connect_pageant()
        .await
        .map(AgentClient::dynamic)
        .map_err(|e| SshError::Key(format!("no SSH agent or Pageant answered: {e}")))
}

/// Keyboard-interactive: the server's rounds of questions, each answered by
/// `answer`, until it lets the user in or not.
async fn interactively(
    handle: &mut Handle<Checker>,
    user: &str,
    answer: &Answerer,
) -> Result<(), SshError> {
    let mut reply = handle
        .authenticate_keyboard_interactive_start(user, None)
        .await
        .map_err(dropped)?;
    loop {
        match reply {
            KeyboardInteractiveAuthResponse::Success => return Ok(()),
            KeyboardInteractiveAuthResponse::Failure { .. } => {
                return Err(SshError::Denied(format!(
                    "those answers aren't the way in for {user}."
                )))
            }
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let questions = Questions {
                    name,
                    instructions,
                    prompts: prompts.into_iter().map(|p| (p.prompt, p.echo)).collect(),
                };
                let count = questions.prompts.len();
                let asked: Vec<String> = (questions.prompts.iter())
                    .map(|(prompt, _)| prompt.trim().trim_end_matches(':').trim().to_string())
                    .filter(|prompt| !prompt.is_empty())
                    .collect();
                let unanswered = || match asked.as_slice() {
                    [] => SshError::Denied("the questions weren't answered.".into()),
                    asked => SshError::Denied(format!(
                        "it asks for {}, which only you can answer: Rovik answers only its \
                         password prompts. Log in with your system ssh to answer it yourself.",
                        asked
                            .iter()
                            .map(|a| format!("\"{a}\""))
                            .collect::<Vec<_>>()
                            .join(" and ")
                    )),
                };
                let answers = answer(questions).await.ok_or_else(unanswered)?;
                if answers.len() != count {
                    return Err(SshError::Denied(
                        "each of the host's questions needs an answer.".into(),
                    ));
                }
                let answers = answers.iter().map(|a| a.0.clone()).collect();
                reply = handle
                    .authenticate_keyboard_interactive_respond(answers)
                    .await
                    .map_err(dropped)?;
            }
        }
    }
}

/// A host's daemon through SSH (D5): one connection, made when the first
/// stream is asked for and again once it drops, and a `direct-streamlocal`
/// channel to the daemon's socket for each stream, or `attach` where the
/// host refuses those.
pub struct SshConnector {
    target: SshTarget,
    /// The daemon's socket there, asked for once.
    socket: tokio::sync::Mutex<Option<String>>,
    attach: String,
    session: tokio::sync::Mutex<Option<Arc<Ssh>>>,
    /// The builds Rovik's binary on the host may be, by their SHA-256,
    /// checked on each new connection, besides its release's.
    known: Option<Vec<String>>,
    releases: Option<crate::release::Releases>,
}

impl SshConnector {
    /// The daemon on `target`, at `~/.rovik/agentd.sock` there.
    pub fn new(target: SshTarget) -> SshConnector {
        SshConnector {
            target,
            socket: tokio::sync::Mutex::new(None),
            attach: ATTACH.into(),
            session: tokio::sync::Mutex::new(None),
            known: None,
            releases: None,
        }
    }

    /// The daemon's socket at `path` there, an absolute path.
    pub fn with_socket(self, path: impl Into<String>) -> SshConnector {
        *self.socket.try_lock().expect("a new connector") = Some(path.into());
        self
    }

    /// `command` in place of `ATTACH`, where the host refuses streamlocal.
    pub fn with_attach(mut self, command: impl Into<String>) -> SshConnector {
        self.attach = command.into();
        self
    }

    /// Checks Rovik's install on each new connection (D5, H05): the
    /// binary must be one of the builds `known` names by their SHA-256, or
    /// one its version's release lists in `releases`, and the user's unit
    /// must run it as installed. A connection that fails is refused,
    /// saying why, and not tried again.
    pub fn with_install_check(
        mut self,
        known: Vec<String>,
        releases: Option<crate::release::Releases>,
    ) -> SshConnector {
        self.known = Some(known);
        self.releases = releases;
        self
    }

    /// Where, and as whom.
    pub fn target(&self) -> &SshTarget {
        &self.target
    }

    /// The daemon's socket there: asked for once, from `$HOME`.
    pub async fn socket_path(&self) -> Result<String, SshError> {
        let mut socket = self.socket.lock().await;
        if let Some(path) = socket.as_ref() {
            return Ok(path.clone());
        }
        let home = self.session().await?.home().await?;
        let path = format!("{}/{SOCKET}", home.trim_end_matches('/'));
        *socket = Some(path.clone());
        Ok(path)
    }

    /// The connection, made again if it dropped.
    pub async fn session(&self) -> Result<Arc<Ssh>, SshError> {
        let mut session = self.session.lock().await;
        if let Some(ssh) = session.as_ref().filter(|ssh| !ssh.is_closed()) {
            return Ok(Arc::clone(ssh));
        }
        let ssh = Arc::new(Ssh::connect(&self.target).await?);
        if let Some(known) = &self.known {
            let layout = crate::deploy::Layout::default();
            let releases = self.releases.as_ref();
            let user = &self.target.user;
            let checked = crate::deploy::check_install(&ssh, known, releases, user, &layout);
            checked.await.map_err(|e| match e {
                // The check couldn't be made now: no verdict on the install.
                crate::deploy::DeployError::Failed(why) => SshError::Failed(why),
                e => SshError::Install(e.to_string()),
            })?;
        }
        *session = Some(Arc::clone(&ssh));
        Ok(ssh)
    }
}

impl Connector for SshConnector {
    fn connect(&self) -> Connecting<'_> {
        Box::pin(async move {
            let socket = self.socket_path().await.map_err(io_error)?;
            let ssh = self.session().await.map_err(io_error)?;
            ssh.stream_to_daemon(&socket, &self.attach)
                .await
                .map_err(io_error)
        })
    }
}

/// The error a `Client` sees (`Connector`). The host turning this computer
/// away is a `Refusal`: the credentials and a host key never pinned wait
/// for the user, as trying again would send the credentials for the same
/// answer; a host key other than the one pinned (the network the computer
/// is on) and Rovik's install there (a release's list of builds not yet
/// published) may come right on their own, and are tried again now and
/// then. The key file and the agent are this computer's (an agent not up
/// yet, a drive not mounted): a link that didn't come up, tried again soon.
/// A key file that's there and can't be used waits for the user, as the
/// credentials do: tried again, it would fail the same way.
fn io_error(e: SshError) -> io::Error {
    let may_pass = match &e {
        SshError::Denied(_) | SshError::UnknownHostKey { .. } | SshError::KeyUnusable(_) => false,
        SshError::HostKeyChanged { .. } | SshError::Install(_) => true,
        SshError::Unreachable(_) | SshError::Failed(_) | SshError::Key(_) => {
            return io::Error::new(io::ErrorKind::ConnectionRefused, e.to_string())
        }
    };
    Refusal {
        why: e.to_string(),
        may_pass,
    }
    .into_error()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the client sees of each failure: the credentials and a key to
    /// pin refuse for good; a changed host key and Rovik's install refuse
    /// for now; the key file, the agent and the network are no refusal.
    #[test]
    fn only_the_hosts_answers_are_refusals_and_some_may_pass() {
        let refusal = |e: SshError| {
            let e = io_error(e);
            Refusal::of(&e).map(|r| r.may_pass)
        };
        assert_eq!(
            refusal(SshError::Denied("that password".into())),
            Some(false)
        );
        let unknown = SshError::UnknownHostKey {
            key: String::new(),
            fingerprint: "SHA256:x".into(),
        };
        assert_eq!(refusal(unknown), Some(false));
        let changed = SshError::HostKeyChanged {
            pinned: "SHA256:a".into(),
            found: "SHA256:b".into(),
        };
        assert_eq!(refusal(changed), Some(true));
        assert_eq!(refusal(SshError::Install("changed".into())), Some(true));
        assert_eq!(
            refusal(SshError::KeyUnusable("id_ed25519: bad passphrase".into())),
            Some(false)
        );
        for passing in [
            SshError::Key("no SSH agent answered: not found".into()),
            SshError::Key("/media/usb/id_ed25519: not found".into()),
            SshError::Unreachable("no route".into()),
            SshError::Failed("dropped".into()),
        ] {
            let shown = passing.to_string();
            assert_eq!(refusal(passing), None, "{shown}");
        }
    }
}

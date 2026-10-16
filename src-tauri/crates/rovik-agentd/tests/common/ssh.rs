//! An SSH server in the test, with russh (H02, H03). `USER` gets in with
//! `PASSWORD`, with `user_key` (from a file or an agent), or by answering
//! its one keyboard-interactive question with `PASSWORD`. Each
//! `direct-streamlocal` channel reaches the Unix socket it names on this
//! machine, the daemon's, and the server counts how many are open at once;
//! one started with `refusing_streamlocal` refuses them, as a server set
//! up so does. An exec channel runs its command with `sh -c` here, its
//! stdin and stdout the channel's; one started `as_user` runs it with that
//! home, and as that account when the test runs as root, and refuses
//! streamlocal, whose connections would come from the test's account.

use russh::keys::agent::client::AgentClient;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey, PublicKey};
use russh::server::{self, Auth, ChannelOpenHandle, Msg, Response, Session};
use russh::{Channel, ChannelId, ChannelMsg, ChannelOpenFailure};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener};

pub const USER: &str = "me";
pub const PASSWORD: &str = "open sesame";

/// Whom an exec channel's commands run as.
#[derive(Clone)]
pub struct Account {
    pub home: PathBuf,
    /// The uid and gid to switch to; none to stay the test's own.
    pub ids: Option<(u32, u32)>,
}

pub struct SshServer {
    pub port: u16,
    pub host_key: PrivateKey,
    /// The key `USER` gets in with.
    pub user_key: PrivateKey,
    /// The most streamlocal channels that were open at once.
    pub most_open: Arc<AtomicUsize>,
    /// The commands run so far.
    pub execs: Arc<AtomicUsize>,
}

impl SshServer {
    pub async fn start() -> SshServer {
        SshServer::serving(false, None).await
    }

    /// A server that refuses streamlocal forwarding.
    pub async fn refusing_streamlocal() -> SshServer {
        SshServer::serving(true, None).await
    }

    /// A server whose commands run as `account`.
    pub async fn as_user(account: Account) -> SshServer {
        SshServer::serving(true, Some(account)).await
    }

    async fn serving(refuse_streamlocal: bool, account: Option<Account>) -> SshServer {
        let host_key = new_key();
        let user_key = new_key();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = Arc::new(server::Config {
            keys: vec![host_key.clone()],
            auth_rejection_time: Duration::from_millis(10),
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        let most_open = Arc::new(AtomicUsize::new(0));
        let execs = Arc::new(AtomicUsize::new(0));
        let (open, most) = (Arc::new(AtomicUsize::new(0)), Arc::clone(&most_open));
        let accepted = user_key.public_key().clone();
        let counted = Arc::clone(&execs);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let side = ServerSide {
                    accepted: accepted.clone(),
                    open: Arc::clone(&open),
                    most: Arc::clone(&most),
                    execs: Arc::clone(&counted),
                    refuse_streamlocal,
                    sessions: HashMap::new(),
                    account: account.clone(),
                };
                let config = Arc::clone(&config);
                tokio::spawn(async move {
                    if let Ok(session) = server::run_stream(config, stream, side).await {
                        let _ = session.await;
                    }
                });
            }
        });
        SshServer {
            port,
            host_key,
            user_key,
            most_open,
            execs,
        }
    }

    /// The host key as OpenSSH writes it: what the app pins.
    pub fn host_key_line(&self) -> String {
        self.host_key.public_key().to_openssh().unwrap()
    }

    /// `user_key` in a file of its own under `dir`, encrypted with
    /// `passphrase`.
    pub fn key_file(&self, dir: &Path, passphrase: &str) -> PathBuf {
        let file = dir.join("id_ed25519");
        let encrypted = self.user_key.encrypt(&mut rand::rng(), passphrase).unwrap();
        let text = encrypted.to_openssh(LineEnding::LF).unwrap();
        std::fs::write(&file, text.as_bytes()).unwrap();
        file
    }

    /// An SSH agent at a socket under `dir`, holding `user_key`.
    pub async fn agent(&self, dir: &Path) -> PathBuf {
        let socket = dir.join("agent.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let incoming = futures_util::stream::unfold(listener, |listener| async move {
            let accepted = listener.accept().await.map(|(stream, _)| stream);
            Some((accepted, listener))
        });
        tokio::spawn(russh::keys::agent::server::serve(Box::pin(incoming), ()));
        let mut agent = AgentClient::connect_uds(&socket).await.unwrap();
        agent.add_identity(&self.user_key, &[]).await.unwrap();
        socket
    }
}

/// A new Ed25519 key.
pub fn new_key() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
}

struct ServerSide {
    accepted: PublicKey,
    open: Arc<AtomicUsize>,
    most: Arc<AtomicUsize>,
    execs: Arc<AtomicUsize>,
    refuse_streamlocal: bool,
    /// The session channels waiting for their command.
    sessions: HashMap<ChannelId, Channel<Msg>>,
    account: Option<Account>,
}

impl server::Handler for ServerSide {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(match user == USER && password == PASSWORD {
            true => Auth::Accept,
            false => Auth::reject(),
        })
    }

    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        let ours = key.key_data() == self.accepted.key_data();
        Ok(match user == USER && ours {
            true => Auth::Accept,
            false => Auth::reject(),
        })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        user: &str,
        _submethods: &str,
        response: Option<Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        let Some(mut answers) = response else {
            return Ok(Auth::Partial {
                name: Cow::Borrowed("Test"),
                instructions: Cow::Borrowed("Answer the question."),
                prompts: Cow::Owned(vec![(Cow::Borrowed("Password: "), false)]),
            });
        };
        let answer = answers.next();
        let right = answer.as_deref() == Some(PASSWORD.as_bytes());
        Ok(match user == USER && right {
            true => Auth::Accept,
            false => Auth::reject(),
        })
    }

    async fn channel_open_direct_streamlocal(
        &mut self,
        channel: Channel<Msg>,
        socket_path: &str,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.refuse_streamlocal {
            reply
                .reject(ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return Ok(());
        }
        let Ok(mut socket) = tokio::net::UnixStream::connect(socket_path).await else {
            reply.reject(ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };
        reply.accept().await;
        let now = self.open.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);
        let open = Arc::clone(&self.open);
        tokio::spawn(async move {
            let mut channel = channel.into_stream();
            let _ = tokio::io::copy_bidirectional(&mut channel, &mut socket).await;
            open.fetch_sub(1, Ordering::SeqCst);
        });
        Ok(())
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        self.sessions.insert(channel.id(), channel);
        Ok(())
    }

    async fn exec_request(
        &mut self,
        id: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Some(channel) = self.sessions.remove(&id) else {
            return session.channel_failure(id);
        };
        session.channel_success(id)?;
        self.execs.fetch_add(1, Ordering::SeqCst);
        let command = String::from_utf8_lossy(data).into_owned();
        let account = self.account.clone();
        tokio::spawn(run(command, channel, session.handle(), account));
        Ok(())
    }
}

/// Runs `command` for an exec channel: what the client sends goes to its
/// stdin; its stdout, then its exit status, go back through the session's
/// handle, in that order.
async fn run(
    command: String,
    mut channel: Channel<Msg>,
    handle: server::Handle,
    account: Option<Account>,
) {
    let id = channel.id();
    let mut sh = tokio::process::Command::new("sh");
    sh.args(["-c", &command])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(account) = account {
        sh.env("HOME", &account.home).env("SHELL", "/bin/sh");
        if let Some((uid, gid)) = account.ids {
            sh.uid(uid).gid(gid);
        }
    }
    let spawned = sh.spawn();
    let Ok(mut child) = spawned else {
        let _ = handle.exit_status_request(id, 127).await;
        let _ = handle.close(id).await;
        return;
    };
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    // What it says on stderr goes back as the channel's extended data.
    let mut stderr = child.stderr.take().unwrap();
    let errors = handle.clone();
    let said = tokio::spawn(async move {
        let mut buffer = vec![0; 8 * 1024];
        while let Ok(n) = stderr.read(&mut buffer).await {
            if n == 0
                || errors
                    .extended_data(id, 1, buffer[..n].to_vec())
                    .await
                    .is_err()
            {
                break;
            }
        }
    });
    tokio::spawn(async move {
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => {
                    if stdin.write_all(&data).await.is_err() {
                        break;
                    }
                }
                ChannelMsg::Eof | ChannelMsg::Close => break,
                _ => {}
            }
        }
        let _ = stdin.shutdown().await;
    });
    let mut buffer = vec![0; 32 * 1024];
    while let Ok(n) = stdout.read(&mut buffer).await {
        if n == 0 || handle.data(id, buffer[..n].to_vec()).await.is_err() {
            break;
        }
    }
    let _ = said.await;
    let status = child.wait().await.ok().and_then(|s| s.code()).unwrap_or(1);
    let _ = handle.exit_status_request(id, status as u32).await;
    let _ = handle.eof(id).await;
    let _ = handle.close(id).await;
}

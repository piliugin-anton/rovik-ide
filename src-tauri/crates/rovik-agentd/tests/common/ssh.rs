//! An SSH server in the test, with russh (H02). `USER` gets in with
//! `PASSWORD`, with `user_key` (from a file or an agent), or by answering
//! its one keyboard-interactive question with `PASSWORD`. Each
//! `direct-streamlocal` channel reaches the Unix socket it names on this
//! machine, the daemon's, and the server counts how many are open at once.
//! A command sent on an exec channel comes back as its output.

use russh::keys::agent::client::AgentClient;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey, PublicKey};
use russh::server::{self, Auth, ChannelOpenHandle, Msg, Response, Session};
use russh::{Channel, ChannelId};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, UnixListener};

pub const USER: &str = "me";
pub const PASSWORD: &str = "open sesame";

pub struct SshServer {
    pub port: u16,
    pub host_key: PrivateKey,
    /// The key `USER` gets in with.
    pub user_key: PrivateKey,
    /// The most streamlocal channels that were open at once.
    pub most_open: Arc<AtomicUsize>,
}

impl SshServer {
    pub async fn start() -> SshServer {
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
        let (open, most) = (Arc::new(AtomicUsize::new(0)), Arc::clone(&most_open));
        let accepted = user_key.public_key().clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let side = ServerSide {
                    accepted: accepted.clone(),
                    open: Arc::clone(&open),
                    most: Arc::clone(&most),
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
        let Ok(mut socket) = tokio::net::UnixStream::connect(socket_path).await else {
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
        _channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.data(channel, data.to_vec())?;
        session.exit_status_request(channel, 0)?;
        session.eof(channel)?;
        session.close(channel)
    }
}

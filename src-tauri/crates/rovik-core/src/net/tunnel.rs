//! A TCP connection to a cloud host, directly or through a proxy: an HTTP
//! proxy's CONNECT, or a SOCKS5 proxy's. SSH starts with the server
//! talking, so what follows a proxy's answer in the same read is the
//! host's first bytes: the link hands them on before anything else.

use super::settings::authority;
use base64::Engine;
use std::fmt;
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;

/// The most of a proxy's answer to CONNECT that's read.
const MAX_ANSWER: usize = 16 * 1024;

/// How the connection gets to the host.
#[derive(Clone, PartialEq, Eq)]
pub enum Hop {
    Direct,
    /// An HTTP proxy, and its `Proxy-Authorization` when it's signed in to.
    Http {
        host: String,
        port: u16,
        auth: Option<String>,
    },
    /// A SOCKS5 proxy, its username and password when it's signed in to,
    /// and whether it looks the host's name up (socks5h) or this computer
    /// does (socks5).
    Socks5 {
        host: String,
        port: u16,
        auth: Option<(String, String)>,
        remote_names: bool,
    },
}

impl Hop {
    /// An HTTP proxy, signed in to with Basic when there's a username.
    pub fn http(host: &str, port: u16, user: Option<&str>, password: &str) -> Hop {
        Hop::Http {
            host: host.to_string(),
            port,
            auth: user.map(|user| basic(user, password)),
        }
    }
}

/// The value of a Basic `Proxy-Authorization`.
pub fn basic(user: &str, password: &str) -> String {
    let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
    format!("Basic {token}")
}

// No credential in a log.
impl fmt::Debug for Hop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Hop::Direct => f.write_str("Direct"),
            Hop::Http { host, port, auth } => f
                .debug_struct("Http")
                .field("proxy", &authority(host, *port))
                .field("signed_in", &auth.is_some())
                .finish(),
            Hop::Socks5 {
                host,
                port,
                auth,
                remote_names,
            } => f
                .debug_struct("Socks5")
                .field("proxy", &authority(host, *port))
                .field("signed_in", &auth.is_some())
                .field("remote_names", remote_names)
                .finish(),
        }
    }
}

/// A connection to the host: the host's first bytes the proxy's answer
/// came with, then the socket.
pub struct Link {
    early: Vec<u8>,
    taken: usize,
    stream: TcpStream,
}

impl Link {
    fn new(stream: TcpStream, early: Vec<u8>) -> Link {
        let _ = stream.set_nodelay(true);
        Link {
            early,
            taken: 0,
            stream,
        }
    }
}

impl AsyncRead for Link {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if this.taken < this.early.len() {
            let n = buf.remaining().min(this.early.len() - this.taken);
            buf.put_slice(&this.early[this.taken..this.taken + n]);
            this.taken += n;
            if this.taken == this.early.len() {
                this.early = Vec::new();
                this.taken = 0;
            }
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for Link {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

/// Connects to `host:port` by `hop`, the connection and the proxy's
/// handshake together within `timeout`.
pub async fn open(host: &str, port: u16, hop: &Hop, timeout: Duration) -> Result<Link, String> {
    let target = authority(host, port);
    let connect = async {
        match hop {
            Hop::Direct => {
                let stream = TcpStream::connect((host, port))
                    .await
                    .map_err(|e| format!("{target}: {}", plain(&e)))?;
                Ok(Link::new(stream, Vec::new()))
            }
            Hop::Http {
                host: proxy,
                port: proxy_port,
                auth,
            } => {
                let mut stream = reach_proxy(proxy, *proxy_port).await?;
                let early = connect_through_http(&mut stream, &target, auth.as_deref()).await?;
                Ok(Link::new(stream, early))
            }
            Hop::Socks5 {
                host: proxy,
                port: proxy_port,
                auth,
                remote_names,
            } => {
                let mut stream = reach_proxy(proxy, *proxy_port).await?;
                connect_through_socks5(&mut stream, host, port, auth.as_ref(), *remote_names)
                    .await?;
                Ok(Link::new(stream, Vec::new()))
            }
        }
    };
    match tokio::time::timeout(timeout, connect).await {
        Ok(link) => link,
        Err(_) => Err(match hop {
            Hop::Direct => format!("{target} didn't answer within {} s.", timeout.as_secs()),
            Hop::Http { host, port, .. } | Hop::Socks5 { host, port, .. } => format!(
                "The proxy at {} didn't connect to {target} within {} s.",
                authority(host, *port),
                timeout.as_secs()
            ),
        }),
    }
}

async fn reach_proxy(host: &str, port: u16) -> Result<TcpStream, String> {
    TcpStream::connect((host, port)).await.map_err(|e| {
        format!(
            "The proxy at {} can't be reached: {}",
            authority(host, port),
            plain(&e)
        )
    })
}

/// An I/O error as a sentence's end.
fn plain(e: &io::Error) -> String {
    let text = e.to_string();
    let text = text.trim_end_matches('.');
    format!("{text}.")
}

/// CONNECT to `target`; whatever came after the proxy's answer.
async fn connect_through_http(
    stream: &mut TcpStream,
    target: &str,
    auth: Option<&str>,
) -> Result<Vec<u8>, String> {
    let mut request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if let Some(auth) = auth {
        request.push_str("Proxy-Authorization: ");
        request.push_str(auth);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    let lost = |e: io::Error| format!("The proxy closed the connection: {}", plain(&e));
    stream.write_all(request.as_bytes()).await.map_err(lost)?;

    let mut answer = Vec::with_capacity(512);
    let mut chunk = [0u8; 2048];
    let end = loop {
        let n = stream.read(&mut chunk).await.map_err(lost)?;
        if n == 0 {
            return Err("The proxy closed the connection without answering.".into());
        }
        answer.extend_from_slice(&chunk[..n]);
        if let Some(at) = answer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        if answer.len() > MAX_ANSWER {
            return Err("The proxy's answer is too long to be one.".into());
        }
    };
    let head = String::from_utf8_lossy(&answer[..end]);
    let status = head.lines().next().unwrap_or("").trim();
    let code = status
        .strip_prefix("HTTP/")
        .and_then(|rest| rest.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok());
    match code {
        Some(200..=299) => Ok(answer[end..].to_vec()),
        Some(407) if auth.is_some() => {
            Err("The proxy didn't accept its username and password.".into())
        }
        Some(407) => Err("The proxy asks for a username and a password.".into()),
        Some(_) => Err(format!(
            "The proxy refused to connect to {target}: {status}"
        )),
        None => Err(format!(
            "The proxy answered something that isn't HTTP: {}",
            status.chars().take(80).collect::<String>()
        )),
    }
}

/// SOCKS5's greeting, sign-in and CONNECT to `host:port` (RFC 1928, 1929).
async fn connect_through_socks5(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
    auth: Option<&(String, String)>,
    remote_names: bool,
) -> Result<(), String> {
    let lost = |e: io::Error| format!("The proxy closed the connection: {}", plain(&e));
    let methods: &[u8] = if auth.is_some() {
        &[0x00, 0x02]
    } else {
        &[0x00]
    };
    let mut greeting = vec![0x05, methods.len() as u8];
    greeting.extend_from_slice(methods);
    stream.write_all(&greeting).await.map_err(lost)?;
    let mut chosen = [0u8; 2];
    stream.read_exact(&mut chosen).await.map_err(lost)?;
    if chosen[0] != 0x05 {
        return Err("The proxy isn't a SOCKS5 proxy.".into());
    }
    match (chosen[1], auth) {
        (0x00, _) => {}
        (0x02, Some((user, password))) => {
            let mut sign_in = vec![0x01, user.len() as u8];
            sign_in.extend_from_slice(user.as_bytes());
            sign_in.push(password.len() as u8);
            sign_in.extend_from_slice(password.as_bytes());
            stream.write_all(&sign_in).await.map_err(lost)?;
            let mut status = [0u8; 2];
            stream.read_exact(&mut status).await.map_err(lost)?;
            if status[1] != 0x00 {
                return Err("The proxy didn't accept its username and password.".into());
            }
        }
        (0xFF, None) | (0x02, None) => {
            return Err("The proxy asks for a username and a password.".into());
        }
        _ => return Err("The proxy asks to sign in a way Rovik doesn't support.".into()),
    }

    let mut request = vec![0x05, 0x01, 0x00];
    let address = match host.parse::<IpAddr>() {
        Ok(ip) => ip,
        Err(_) if remote_names => {
            if host.len() > 255 {
                return Err(format!("{host} is too long a name for a SOCKS5 proxy."));
            }
            request.push(0x03);
            request.push(host.len() as u8);
            request.extend_from_slice(host.as_bytes());
            request.extend_from_slice(&port.to_be_bytes());
            return socks5_request(stream, &request, host, port).await;
        }
        Err(_) => tokio::net::lookup_host((host, port))
            .await
            .ok()
            .and_then(|mut found| found.next())
            .map(|address| address.ip())
            .ok_or_else(|| format!("{host} isn't a name this computer can look up."))?,
    };
    match address {
        IpAddr::V4(ip) => {
            request.push(0x01);
            request.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            request.push(0x04);
            request.extend_from_slice(&ip.octets());
        }
    }
    request.extend_from_slice(&port.to_be_bytes());
    socks5_request(stream, &request, host, port).await
}

/// Sends the CONNECT and reads its answer to the end.
async fn socks5_request(
    stream: &mut TcpStream,
    request: &[u8],
    host: &str,
    port: u16,
) -> Result<(), String> {
    let lost = |e: io::Error| format!("The proxy closed the connection: {}", plain(&e));
    stream.write_all(request).await.map_err(lost)?;
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await.map_err(lost)?;
    if head[0] != 0x05 {
        return Err("The proxy isn't a SOCKS5 proxy.".into());
    }
    let target = authority(host, port);
    if head[1] != 0x00 {
        return Err(match head[1] {
            0x02 => format!("The proxy's rules don't allow connecting to {target}."),
            0x03 => format!("The proxy can't reach the network {target} is on."),
            0x04 => format!("The proxy can't reach {host}."),
            0x05 => format!("{target} refused the proxy's connection."),
            0x06 => format!("The proxy's connection to {target} timed out."),
            0x07 | 0x08 => "The proxy doesn't connect the way Rovik asks.".to_string(),
            _ => format!("The proxy couldn't connect to {target}."),
        });
    }
    // Where the proxy connected from, which nothing here needs.
    let rest = match head[3] {
        0x01 => 4,
        0x04 => 16,
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await.map_err(lost)?;
            usize::from(len[0])
        }
        _ => return Err("The proxy's answer is garbled.".into()),
    } + 2;
    let mut skipped = vec![0u8; rest];
    stream.read_exact(&mut skipped).await.map_err(lost)?;
    Ok(())
}

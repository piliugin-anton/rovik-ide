//! The allowed hosts' filter (docs/design/allowed-hosts-plan.md, J03): an
//! HTTP proxy of Rovik's own that the agent's commands, browser and
//! connectors reach the network through while the list is on. It passes a
//! connection to a listed host on, and refuses any other with `403` and
//! why, remembering it for the model's notes ([`Denials`]).
//!
//! - **What it speaks**: `CONNECT host:port` (HTTPS, and any TCP a client
//!   tunnels), and plain HTTP in absolute form (`GET http://host/path`),
//!   what the proxy variables make nearly every tool send.
//! - **How it goes on** ([`Upstream`]): the way the client would have gone
//!   without it ([`Routes`]). Every connection is a tunnel to the host it
//!   was checked for, through an upstream proxy's `CONNECT` too, and a
//!   plain HTTP request goes on with `Connection: close`: nothing a client
//!   sends on the same connection reaches another host.
//! - **This computer's own services** are never reached through it: there
//!   they'd be the filter's own loopback, which on a cloud host is every
//!   user's. Clients reach them directly (`NO_PROXY`).
//! - **Who asked**: a client may name its chat as the proxy's user
//!   (`http://<chat>@127.0.0.1:port`); nothing is asked for, nothing is
//!   checked, it only says whose a refusal was.

use super::AllowedHosts;
use crate::net::{Hop, Link};
use base64::Engine;
use futures_util::future::BoxFuture;
use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The longest request head the filter reads.
const MAX_HEAD: usize = 64 * 1024;

/// The most headers a request may have.
const MAX_HEADERS: usize = 100;

/// How many refusals are remembered, the newest kept.
const MAX_DENIALS: usize = 256;

/// Who a filter serves, which says how its connections go on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Client {
    /// The agent's commands and the connectors' programs: on the way the
    /// system's proxy says, as their environment would have them go.
    Commands,
    /// The agent's browser: on the way Settings → Network's browser row
    /// says.
    Browser,
}

/// The list a filter checks, as it is now: None, any host.
pub type ListNow = Arc<dyn Fn() -> Option<Arc<AllowedHosts>> + Send + Sync>;

/// Why a connection to a host the list allows wasn't made.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The filter won't make it: a `403`, remembered.
    Denied(String),
    /// It couldn't be made: a `502`.
    Failed(String),
}

/// How a filter's connections go on.
pub trait Upstream: Send + Sync {
    /// A connection to `host:port`, a host the list allows.
    fn open<'a>(&'a self, host: &'a str, port: u16) -> BoxFuture<'a, Result<Link, Refusal>>;
}

/// A host the filter refused.
#[derive(Clone, Debug)]
struct Denial {
    host: String,
    at: Instant,
    client: Client,
    /// The chat the client named, if it named one.
    chat: Option<String>,
}

/// The filter's refusals, newest last, for the model's notes.
#[derive(Default)]
pub struct Denials(Mutex<VecDeque<Denial>>);

impl Denials {
    fn add(&self, host: &str, client: Client, chat: Option<String>) {
        let mut all = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if all.len() == MAX_DENIALS {
            all.pop_front();
        }
        all.push_back(Denial {
            host: host.to_string(),
            at: Instant::now(),
            client,
            chat,
        });
    }

    /// The hosts refused to `client` since `since`, each once, in order:
    /// for `chat`, those it named as its own and those no client named.
    pub fn since(&self, since: Instant, client: Client, chat: Option<&str>) -> Vec<String> {
        let all = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut hosts: Vec<String> = Vec::new();
        for denial in all.iter() {
            let theirs = match (&denial.chat, chat) {
                (Some(named), Some(chat)) => named == chat,
                _ => true,
            };
            if denial.at >= since
                && denial.client == client
                && theirs
                && !hosts.contains(&denial.host)
            {
                hosts.push(denial.host.clone());
            }
        }
        hosts
    }
}

/// A filter: whom it serves, the list it checks, how it goes on, and where
/// it remembers what it refused.
pub struct Filter {
    pub client: Client,
    pub list: ListNow,
    pub upstream: Arc<dyn Upstream>,
    pub denials: Arc<Denials>,
}

impl Filter {
    /// Serves the clients `listener` takes, each connection on its own,
    /// until the listener fails.
    pub async fn serve(self: Arc<Self>, listener: TcpListener) {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let filter = self.clone();
                    crate::rt::spawn(async move { filter.handle(stream).await });
                }
                Err(e) => {
                    log::error!("the allowed hosts' filter stopped taking connections: {e}");
                    return;
                }
            }
        }
    }

    /// One client's connection: its request checked, then carried on, or
    /// answered with why not.
    async fn handle(&self, mut inbound: TcpStream) {
        let Ok(bytes) = read_head(&mut inbound).await else {
            return;
        };
        let request = match Request::parse(&bytes) {
            Ok(request) => request,
            Err(why) => return answer(inbound, "400 Bad Request", &why).await,
        };
        if let Some(why) = self.refusal(&request.host) {
            self.denials
                .add(&request.host, self.client, request.chat.clone());
            return answer(inbound, "403 Forbidden", &why).await;
        }
        let mut upstream = match self.upstream.open(&request.host, request.port).await {
            Ok(link) => link,
            Err(Refusal::Denied(why)) => {
                self.denials
                    .add(&request.host, self.client, request.chat.clone());
                return answer(inbound, "403 Forbidden", &why).await;
            }
            Err(Refusal::Failed(why)) => return answer(inbound, "502 Bad Gateway", &why).await,
        };
        let sent = match &request.forward {
            // A tunnel: the client speaks to the host from here on.
            None => {
                inbound
                    .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .await
            }
            Some(head) => upstream.write_all(head).await,
        };
        let after = &bytes[request.head_len..];
        if sent.is_err() || upstream.write_all(after).await.is_err() {
            return;
        }
        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut upstream).await;
    }

    /// Why `host` isn't reached through the filter, if it isn't: it's this
    /// computer's own, or the list doesn't have it.
    fn refusal(&self, host: &str) -> Option<String> {
        if super::is_loopback(host) {
            return Some(format!(
                "Rovik's filter for the allowed hosts doesn't carry connections to this computer's \
                 own services ({host}): connect to them directly (NO_PROXY lists localhost).\n"
            ));
        }
        let list = (self.list)()?;
        (!list.allows(host)).then(|| {
            format!(
                "{host} isn't in the user's allowed hosts ({}): the agent's work reaches only those, \
                 as set in Settings → Permissions & approval → Network.\n",
                list.shown()
            )
        })
    }
}

/// What a client asked the filter for.
#[derive(Debug, PartialEq, Eq)]
struct Request {
    host: String,
    port: u16,
    /// A plain HTTP request's head as the host gets it (origin form, one
    /// request per connection); None for a tunnel (`CONNECT`).
    forward: Option<Vec<u8>>,
    /// How long the head was: what follows it is the client's next bytes.
    head_len: usize,
    /// The chat the client named as the proxy's user.
    chat: Option<String>,
}

impl Request {
    /// The request whose head `bytes` start with; why not, when it isn't
    /// one the filter carries.
    fn parse(bytes: &[u8]) -> Result<Request, String> {
        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut parsed = httparse::Request::new(&mut headers);
        let head_len = match parsed.parse(bytes) {
            Ok(httparse::Status::Complete(len)) => len,
            _ => return Err("That isn't an HTTP request a proxy takes.\n".into()),
        };
        let method = parsed.method.unwrap_or_default();
        let target = parsed.path.unwrap_or_default();
        let chat = parsed
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("proxy-authorization"))
            .and_then(|h| proxy_user(h.value));
        if method.eq_ignore_ascii_case("CONNECT") {
            let (host, port) = authority(target).ok_or_else(|| {
                format!("CONNECT takes a host and a port (host:443), not {target}.\n")
            })?;
            return Ok(Request {
                host,
                port,
                forward: None,
                head_len,
                chat,
            });
        }
        let url = reqwest::Url::parse(target)
            .ok()
            .filter(|url| url.scheme() == "http" && url.host_str().is_some())
            .ok_or_else(|| {
                "This is Rovik's filter for the agent's allowed hosts: it takes CONNECT, and \
                 plain HTTP requests with the whole URL (GET http://host/path).\n"
                    .to_string()
            })?;
        let host = url.host_str().unwrap_or_default();
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string();
        let port = url.port_or_known_default().unwrap_or(80);
        let mut origin = url.path().to_string();
        if let Some(query) = url.query() {
            origin.push('?');
            origin.push_str(query);
        }
        let version = if parsed.version == Some(0) {
            "HTTP/1.0"
        } else {
            "HTTP/1.1"
        };
        let mut head = format!("{method} {origin} {version}\r\n").into_bytes();
        let mut has_host = false;
        for header in parsed.headers.iter() {
            let name = header.name.to_ascii_lowercase();
            // The client's words to the proxy, and how long it keeps the
            // connection: the filter's to decide (one request).
            if matches!(
                name.as_str(),
                "proxy-authorization" | "proxy-connection" | "connection" | "keep-alive"
            ) {
                continue;
            }
            has_host |= name == "host";
            head.extend_from_slice(header.name.as_bytes());
            head.extend_from_slice(b": ");
            head.extend_from_slice(header.value);
            head.extend_from_slice(b"\r\n");
        }
        if !has_host {
            head.extend_from_slice(format!("Host: {}\r\n", authority_of(&url)).as_bytes());
        }
        head.extend_from_slice(b"Connection: close\r\n\r\n");
        Ok(Request {
            host,
            port,
            forward: Some(head),
            head_len,
            chat,
        })
    }
}

/// `url`'s host and port as a Host header has them.
fn authority_of(url: &reqwest::Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    }
}

/// A CONNECT's `host:port` (`[::1]:443` for an IPv6 address).
fn authority(target: &str) -> Option<(String, u16)> {
    let url = reqwest::Url::parse(&format!("http://{target}/")).ok()?;
    // The URL fills in 80 for a missing port: CONNECT names its own.
    let (_, port) = target.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let host = url.host_str()?;
    Some((
        host.trim_start_matches('[')
            .trim_end_matches(']')
            .to_string(),
        port,
    ))
}

/// The user a `Proxy-Authorization: Basic …` names, if it names one.
fn proxy_user(value: &[u8]) -> Option<String> {
    let value = std::str::from_utf8(value).ok()?.trim();
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(token.trim())
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let user = decoded.split(':').next()?;
    let user = percent_decode(user);
    (!user.is_empty()).then_some(user)
}

/// `text` with its `%XX` escapes decoded, as a URL's user is written.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A request's head, and whatever came after it in the same reads; an
/// error when the client closed first or the head is too long.
async fn read_head(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        let searched_from = bytes.len().saturating_sub(3);
        bytes.extend_from_slice(&chunk[..n]);
        if bytes[searched_from..].windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(bytes);
        }
        if bytes.len() > MAX_HEAD {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
    }
}

/// Answers the client with `status` and `body`, then closes.
async fn answer(mut stream: TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// The way a client's connections go on in production: the browser's by
/// Settings → Network's browser row, the commands' by the system's proxy
/// ([`crate::net::agent_hop`]). Going directly, a name is looked up here
/// and refused when it leads to this computer, the unspecified address or
/// a link-local one (a cloud's metadata service): a listed name can't be
/// pointed back at what the filter keeps out.
pub struct Routes(pub Client);

impl Upstream for Routes {
    fn open<'a>(&'a self, host: &'a str, port: u16) -> BoxFuture<'a, Result<Link, Refusal>> {
        Box::pin(async move {
            let browser = self.0 == Client::Browser;
            let hop = crate::net::agent_hop(browser, host, port)
                .await
                .map_err(Refusal::Failed)?;
            let timeout = crate::net::connect_timeout().await;
            match hop {
                Hop::Direct => open_direct(host, port, timeout).await,
                hop => crate::net::open_tunnel(host, port, &hop, timeout)
                    .await
                    .map_err(Refusal::Failed),
            }
        })
    }
}

/// A direct connection to `host:port`: a name looked up here, and refused
/// when it leads where [`reachable`] says the filter doesn't go.
async fn open_direct(host: &str, port: u16, timeout: Duration) -> Result<Link, Refusal> {
    let literal = host.parse::<IpAddr>().ok();
    let addresses: Vec<SocketAddr> = match literal {
        Some(ip) => vec![SocketAddr::new(ip, port)],
        None => tokio::net::lookup_host((host, port))
            .await
            .map_err(|e| Refusal::Failed(format!("{host} couldn't be looked up: {e}.\n")))?
            .collect(),
    };
    if literal.is_none() {
        if let Some(address) = addresses.iter().find(|a| !reachable(a.ip())) {
            return Err(Refusal::Denied(format!(
                "{host} leads to {}, which Rovik's filter for the allowed hosts doesn't reach: \
                 this computer's own, or a link-local address.\n",
                address.ip()
            )));
        }
    }
    let mut last = format!("{host} has no address.\n");
    for address in addresses {
        match crate::net::open_tunnel(&address.ip().to_string(), port, &Hop::Direct, timeout).await
        {
            Ok(link) => return Ok(link),
            Err(e) => last = format!("{e}\n"),
        }
    }
    Err(Refusal::Failed(last))
}

/// Whether the filter goes to `ip` for a listed name: not this computer's
/// loopback, the unspecified address, or a link-local one.
fn reachable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !(v4.is_loopback() || v4.is_unspecified() || v4.is_link_local()),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => reachable(IpAddr::V4(v4)),
            None => !(v6.is_loopback() || v6.is_unspecified() || v6.is_unicast_link_local()),
        },
    }
}

#[cfg(test)]
mod tests;

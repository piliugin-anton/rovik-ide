//! What the crate's tests share: no daemon at all, and a daemon of the
//! tests' own, which answers every call and keeps the requests.

use crate::client::Identity;
use crate::connector::{Connecting, Connector, Stream};
use futures_util::{SinkExt, StreamExt};
use rovik_protocol::agent::AgentStatus;
use rovik_protocol::host::{HostInfo, Storage};
use rovik_protocol::{Fail, FailCode, Frame, Request, Response, Secret, FORWARD_UPGRADE, PROTOCOL};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// No daemon there.
pub struct Nowhere;

impl Connector for Nowhere {
    fn connect(&self) -> Connecting<'_> {
        Box::pin(async { Err(std::io::ErrorKind::ConnectionRefused.into()) })
    }
}

pub fn identity(host: &str) -> Identity {
    Identity {
        host: host.into(),
        computer: "laptop".into(),
        token: Secret("token".into()),
        client: "rovik-client tests".into(),
    }
}

/// A daemon on the other end of each connection: it welcomes, subscribes,
/// and answers each call as a daemon would when all goes well, keeping
/// the requests in order.
#[derive(Clone, Default)]
pub struct Fake {
    pub requests: Arc<Mutex<Vec<Request>>>,
    /// What it answers `HostInfo` with, once set.
    pub info: Arc<Mutex<Option<HostInfo>>>,
    /// What its welcome says it does: nothing unless told.
    pub features: Arc<Mutex<Vec<String>>>,
    /// What it answers `AgentStatus` with, once set: nothing going on
    /// until then.
    pub status: Arc<Mutex<Option<AgentStatus>>>,
    /// What it answers `Storage` with, then `CleanUp` (what's left after
    /// the cleanup), once set.
    pub storage: Arc<Mutex<Option<(Storage, Storage)>>>,
    /// The user's private network there (D20): the address of what
    /// listens on each of its ports, a server of the tests' own. `/forward`
    /// leads to it.
    pub network: Arc<Mutex<HashMap<u16, SocketAddr>>>,
    /// It answers `PageOpened` that the page isn't wanted any more (its
    /// command stopped waiting), once set.
    pub unwanted_pages: Arc<AtomicBool>,
}

impl Connector for Fake {
    fn connect(&self) -> Connecting<'_> {
        let fake = self.clone();
        Box::pin(async move {
            let (client, server) = tokio::io::duplex(1 << 16);
            tokio::spawn(serve(server, fake));
            Ok(Box::new(client) as Stream)
        })
    }
}

/// One connection: the WebSocket at `/rpc`, or a byte stream into the
/// private network at `/forward/{port}`.
async fn serve(mut io: DuplexStream, fake: Fake) {
    let Some(head) = read_head(&mut io).await else {
        return;
    };
    let forwarded = head
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("GET /forward/"))
        .and_then(|rest| rest.split(' ').next()?.parse::<u16>().ok());
    if let Some(port) = forwarded {
        return forward(io, &head, port, &fake).await;
    }
    let Some(key) = header(&head, "sec-websocket-key") else {
        return;
    };
    let accept = derive_accept_key(key.as_bytes());
    let switched = format!(
        "HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: upgrade\r\n\
         sec-websocket-accept: {accept}\r\n\r\n"
    );
    if io.write_all(switched.as_bytes()).await.is_err() {
        return;
    }
    let mut socket = WebSocketStream::from_raw_socket(io, Role::Server, None).await;
    let features = fake.features.lock().unwrap().clone();
    let send = |frame: &Frame| Message::Text(serde_json::to_string(frame).unwrap().into());
    while let Some(Ok(Message::Text(text))) = socket.next().await {
        let answer = match serde_json::from_str::<Frame>(&text) {
            Ok(Frame::Hello { .. }) => Frame::Welcome {
                protocol: PROTOCOL,
                min_protocol: PROTOCOL,
                daemon: "fake".into(),
                boot: "boot".into(),
                features: features.clone(),
            },
            Ok(Frame::Subscribe { .. }) => Frame::Subscribed {
                replayed: 0,
                complete: true,
            },
            Ok(Frame::Call { id, req }) => {
                let info = fake.info.lock().unwrap().clone();
                let status = fake.status.lock().unwrap().clone();
                let measured = fake.storage.lock().unwrap().clone();
                let res = Ok(match (&req, info, status, measured) {
                    (Request::HostInfo, Some(info), _, _) => Response::HostInfo(info),
                    (Request::AgentStatus, _, Some(status), _) => Response::AgentStatus(status),
                    (Request::Storage, _, _, Some((before, _))) => Response::Storage(before),
                    (Request::CleanUp, _, _, Some((_, after))) => Response::Storage(after),
                    (Request::PageOpened { .. }, ..) => {
                        Response::Bool(!fake.unwanted_pages.load(Ordering::Relaxed))
                    }
                    _ => answer(&req),
                });
                fake.requests.lock().unwrap().push(req);
                Frame::Reply { id, res }
            }
            _ => continue,
        };
        if socket.send(send(&answer)).await.is_err() {
            return;
        }
    }
}

/// `/forward/{port}`, as the daemon answers it: upgraded to a byte stream
/// to what listens on `port` in the fake's network, or refused with why.
async fn forward(mut io: DuplexStream, head: &str, port: u16, fake: &Fake) {
    let refuse = |status: &str, fail: Fail| {
        let body = serde_json::to_string(&fail).unwrap();
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        )
    };
    if header(head, "authorization") != Some("Bearer token") {
        let refused = refuse(
            "401 Unauthorized",
            Fail::new(FailCode::BadRequest, "No token."),
        );
        let _ = io.write_all(refused.as_bytes()).await;
        return;
    }
    if !header(head, "upgrade").is_some_and(|u| u.eq_ignore_ascii_case(FORWARD_UPGRADE)) {
        let fail = Fail::new(FailCode::BadRequest, "/forward is a byte stream.");
        let _ = io
            .write_all(refuse("400 Bad Request", fail).as_bytes())
            .await;
        return;
    }
    let listening = fake.network.lock().unwrap().get(&port).copied();
    let inside = match listening {
        Some(address) => tokio::net::TcpStream::connect(address).await.ok(),
        None => None,
    };
    let Some(mut inside) = inside else {
        let why = format!("Nothing listens on port {port} in this user's private network.");
        let refused = refuse("502 Bad Gateway", Fail::new(FailCode::Unreachable, why));
        let _ = io.write_all(refused.as_bytes()).await;
        return;
    };
    let switched =
        format!("HTTP/1.1 101 Switching Protocols\r\nconnection: upgrade\r\nupgrade: {FORWARD_UPGRADE}\r\n\r\n");
    if io.write_all(switched.as_bytes()).await.is_ok() {
        let _ = tokio::io::copy_bidirectional(&mut io, &mut inside).await;
    }
}

/// A request's head, up to its blank line: none if the link ends first.
async fn read_head(io: &mut DuplexStream) -> Option<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 << 10 || io.read(&mut byte).await.ok()? == 0 {
            return None;
        }
        head.push(byte[0]);
    }
    String::from_utf8(head).ok()
}

/// The value of the head's header `name`, whatever its case.
fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

fn answer(request: &Request) -> Response {
    match request {
        Request::AgentStart(_) => Response::RunId(Some("run-1".into())),
        Request::AgentStatus => Response::AgentStatus(AgentStatus::default()),
        Request::AgentStop { .. }
        | Request::AgentPause { .. }
        | Request::AgentResolveApproval { .. }
        | Request::AgentAnswerQuestions { .. } => Response::Bool(true),
        Request::DeliveryTest { remote } => {
            Response::DeliveryTested(rovik_protocol::autopilot::DeliveryTest {
                remote: remote.clone(),
                branch: "rovik/test-20261017-095301".into(),
                ..Default::default()
            })
        }
        Request::KeysList => Response::Keys(Vec::new()),
        Request::Storage => Response::Storage(Default::default()),
        Request::McpSecretsKept => Response::Count(1),
        Request::McpSignIn { .. } => Response::Bool(true),
        Request::McpTest { .. } => Response::McpTested(rovik_protocol::mcp::Tested {
            server: "fake 1".into(),
            protocol: "2025-06-18".into(),
            tools: Vec::new(),
        }),
        _ => Response::Done,
    }
}

//! One host's connection (4.1): the WebSocket that carries the calls and
//! the events, `POST /call` for what is too big for a frame, and a new
//! connection whenever the link drops, waiting longer each time (6.10).

use crate::connector::Connector;
use crate::http;
use futures_util::{SinkExt, Stream, StreamExt};
use rovik_protocol::{
    Event, Fail, FailCode, Frame, Request, Response, Secret, MAX_CALL, MAX_FRAME, PROTOCOL,
};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::tungstenite::{self, ClientRequestBuilder, Message};

/// How long a call waits for its reply, unless it says otherwise.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the daemon has to answer `Hello`.
const WELCOME_WAIT: Duration = Duration::from_secs(10);
/// After a lost link, the client waits this long before connecting again,
/// twice as long after each failed try, up to `MAX_BACKOFF` (6.10).
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// The wall clock this far ahead of the monotonic one, which stops while
/// the computer sleeps: it slept, and its link likely died meanwhile, so
/// the client connects again at once (6.10). Looked at every `CLOCK_TICK`.
const SLEPT: Duration = Duration::from_secs(30);
const CLOCK_TICK: Duration = Duration::from_secs(5);

/// Who connects, to which host.
#[derive(Clone, Debug)]
pub struct Identity {
    /// The host's name, for the user: "<host> isn't reachable."
    pub host: String,
    /// This computer's id (D5), and the token that names it.
    pub computer: String,
    pub token: Secret,
    /// The app and its version, for the daemon's log.
    pub client: String,
}

/// A host's connection, as the hosts page shows it.
#[derive(Serialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum HostState {
    #[default]
    Connecting,
    Ready,
    /// Not reachable: the client tries again, waiting longer each time.
    Offline,
    /// The daemon serves only apps newer than this one (4.5).
    VersionMismatch,
}

/// What the daemon said of itself, in its `Welcome`.
#[derive(Clone, Debug, PartialEq)]
pub struct DaemonInfo {
    pub protocol: u32,
    pub min_protocol: u32,
    /// "rovik-agentd 0.1.0"
    pub daemon: String,
    /// The additive requests it serves (4.5).
    pub features: Vec<String>,
}

/// What a client hears from its daemon, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    State(HostState),
    /// Subscribed, after each connection. Not `complete`: this client
    /// missed events (its first connection, a daemon started again since,
    /// or more than the daemon kept), so its chats catch up from the
    /// database (4.3). The events that follow go on from there.
    Subscribed {
        complete: bool,
    },
    Event {
        seq: u64,
        event: Event,
    },
}

/// Where a client's `Incoming` go: called on its own task, in order.
pub type Sink = Arc<dyn Fn(Incoming) + Send + Sync>;

/// One host's connection. Its clones share it, and it closes with the
/// last of them.
#[derive(Clone)]
pub struct Client {
    shared: Arc<Shared>,
    /// Dropped with the last clone, which ends the connection's task.
    _alive: Arc<oneshot::Sender<()>>,
}

struct Shared {
    connector: Box<dyn Connector>,
    identity: Identity,
    sink: Sink,
    next_id: AtomicU64,
    link: Mutex<Link>,
}

#[derive(Default)]
struct Link {
    state: HostState,
    daemon: Option<DaemonInfo>,
    /// The session's writer, which takes frames as text; none while
    /// there's no session.
    writer: Option<mpsc::UnboundedSender<String>>,
    /// Calls waiting for their reply on the WebSocket.
    waiting: HashMap<u64, oneshot::Sender<Result<Response, Fail>>>,
    /// The last event's boot and number, where the next `Subscribe` goes
    /// on from.
    since: Option<(String, u64)>,
}

impl Client {
    /// Connects to the daemon `connector` reaches, and again whenever the
    /// link drops, until the last clone goes. Runs on the current Tokio
    /// runtime.
    pub fn connect(connector: impl Connector, identity: Identity, sink: Sink) -> Client {
        let shared = Arc::new(Shared {
            connector: Box::new(connector),
            identity,
            sink,
            next_id: AtomicU64::new(1),
            link: Mutex::new(Link::default()),
        });
        let (alive, stop) = oneshot::channel();
        tokio::spawn(run(shared.clone(), stop));
        Client {
            shared,
            _alive: Arc::new(alive),
        }
    }

    pub fn state(&self) -> HostState {
        self.shared.lock().state
    }

    /// The daemon's `Welcome`, once one came.
    pub fn daemon(&self) -> Option<DaemonInfo> {
        self.shared.lock().daemon.clone()
    }

    /// The daemon's answer, within `CALL_TIMEOUT`.
    pub async fn call(&self, request: Request) -> Result<Response, Fail> {
        self.call_within(request, CALL_TIMEOUT).await
    }

    /// The daemon's answer as the value it carries (`HostInfo`, `bool`…).
    pub async fn request<T>(&self, request: Request) -> Result<T, Fail>
    where
        T: TryFrom<Response, Error = Fail>,
    {
        T::try_from(self.call(request).await?)
    }

    /// The daemon's answer, within `timeout`. A call too big for a frame
    /// goes through `POST /call`, and so does one whose reply was (4.1).
    pub async fn call_within(&self, request: Request, timeout: Duration) -> Result<Response, Fail> {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let call = serde_json::to_string(&Frame::Call { id, req: request })
            .map_err(|e| Fail::new(FailCode::Internal, e.to_string()))?;
        let host = &self.shared.identity.host;
        if call.len() > MAX_CALL {
            return Err(Fail::new(
                FailCode::TooLarge,
                format!(
                    "This message is too big to send to {host}: {} MB, {} MB at most.",
                    call.len().div_ceil(1 << 20),
                    MAX_CALL >> 20
                ),
            ));
        }
        let reply = async {
            if call.len() > MAX_FRAME {
                return self.post_call(call).await;
            }
            match self.over_websocket(id, call.clone()).await {
                // Only a read's reply is that big, so asking again is safe.
                Err(fail) if fail.code == FailCode::TooLarge => self.post_call(call).await,
                reply => reply,
            }
        };
        let late = || {
            self.shared.lock().waiting.remove(&id);
            Fail::new(
                FailCode::Unreachable,
                format!("{host} didn't answer in time."),
            )
        };
        tokio::time::timeout(timeout, reply)
            .await
            .unwrap_or_else(|_| Err(late()))
    }

    async fn over_websocket(&self, id: u64, call: String) -> Result<Response, Fail> {
        let (tx, rx) = oneshot::channel();
        {
            let mut link = self.shared.lock();
            let sent = link.writer.as_ref().is_some_and(|w| w.send(call).is_ok());
            if !sent {
                return Err(unreachable(&self.shared.identity.host));
            }
            // Under the lock, so the reply can't come before it's waited for.
            link.waiting.insert(id, tx);
        }
        // Dropped unanswered: the session ended.
        rx.await
            .unwrap_or_else(|_| Err(unreachable(&self.shared.identity.host)))
    }

    async fn post_call(&self, call: String) -> Result<Response, Fail> {
        http::post_call(&*self.shared.connector, &self.shared.identity, call).await
    }
}

/// What the app says of a host it can't reach (4.1).
pub(crate) fn unreachable(host: &str) -> Fail {
    Fail::new(
        FailCode::Unreachable,
        format!("{host} isn't reachable. This chat is read-only until it is."),
    )
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Link> {
        self.link.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_state(&self, state: HostState) {
        let before = std::mem::replace(&mut self.lock().state, state);
        if before != state {
            (self.sink)(Incoming::State(state));
        }
    }

    /// Where to subscribe from on a daemon of this boot, and whether that
    /// goes on from the last event. A daemon started again since is
    /// subscribed from its first event: it published what it did before
    /// anyone could connect, such as the runs it restarted (5.4).
    fn resume_from(&self, boot: &str) -> (Option<(String, u64)>, bool) {
        let mut link = self.lock();
        let same = link.since.as_ref().is_some_and(|(b, _)| b == boot);
        if link.since.is_some() && !same {
            link.since = Some((boot.to_string(), 0));
        }
        (link.since.clone(), same)
    }

    /// The session ended: its calls fail as unreachable.
    fn end_session(&self) {
        let mut link = self.lock();
        link.writer = None;
        link.waiting.clear();
    }
}

/// How a session ended.
enum Ended {
    /// The daemon serves only newer apps.
    Refused,
    /// The link dropped, or never came up; `served` when it was ready.
    Lost { served: bool },
    /// The computer slept.
    Slept,
}

/// Sessions, one after another, until the client goes.
async fn run(shared: Arc<Shared>, mut stop: oneshot::Receiver<()>) {
    let mut wait = FIRST_BACKOFF;
    loop {
        shared.set_state(HostState::Connecting);
        let ended = tokio::select! {
            ended = session(&shared) => ended,
            _ = slept(CLOCK_TICK, SystemTime::now) => Ended::Slept,
            _ = &mut stop => break,
        };
        shared.end_session();
        match ended {
            Ended::Refused => shared.set_state(HostState::VersionMismatch),
            Ended::Lost { served } => {
                shared.set_state(HostState::Offline);
                if served {
                    wait = FIRST_BACKOFF;
                }
            }
            Ended::Slept => {
                wait = FIRST_BACKOFF;
                continue;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = slept(CLOCK_TICK, SystemTime::now) => {}
            _ = &mut stop => break,
        }
        wait = (wait * 2).min(MAX_BACKOFF);
    }
    shared.end_session();
}

/// Once the computer has slept: `wall` went `SLEPT` further than the
/// monotonic clock between two looks, a `tick` apart.
async fn slept(tick: Duration, wall: impl Fn() -> SystemTime) {
    let mut last = (Instant::now(), wall());
    loop {
        tokio::time::sleep(tick).await;
        let now = (Instant::now(), wall());
        let awake = now.0 - last.0;
        let passed = now.1.duration_since(last.1).unwrap_or_default();
        if passed > awake + SLEPT {
            return;
        }
        last = now;
    }
}

/// One WebSocket session: `Hello`, the daemon's `Welcome`, `Subscribe`,
/// then the frames that come, until the link drops.
async fn session(shared: &Shared) -> Ended {
    let lost = Ended::Lost { served: false };
    let identity = &shared.identity;
    let Ok(io) = shared.connector.connect().await else {
        return lost;
    };
    let uri: Uri = "ws://rovik-agentd/rpc".parse().expect("a URI");
    let request = ClientRequestBuilder::new(uri)
        .with_header("Authorization", format!("Bearer {}", identity.token.0));
    let Ok((mut socket, _)) = tokio_tungstenite::client_async(request, io).await else {
        return lost;
    };
    let hello = Frame::Hello {
        protocol: PROTOCOL,
        client: identity.client.clone(),
        device: identity.computer.clone(),
        token: identity.token.clone(),
    };
    if socket.send(text(&hello)).await.is_err() {
        return lost;
    }
    let welcome = tokio::time::timeout(WELCOME_WAIT, next_frame(&mut socket)).await;
    let Ok(Some(Frame::Welcome {
        protocol,
        min_protocol,
        daemon,
        boot,
        features,
    })) = welcome
    else {
        return lost;
    };
    shared.lock().daemon = Some(DaemonInfo {
        protocol,
        min_protocol,
        daemon,
        features,
    });
    if PROTOCOL < min_protocol {
        return Ended::Refused;
    }

    let (since, same_boot) = shared.resume_from(&boot);
    let (mut sink, mut stream) = socket.split();
    let (writer, mut frames) = mpsc::unbounded_channel::<String>();
    let subscribe = serde_json::to_string(&Frame::Subscribe { since }).expect("a frame");
    let _ = writer.send(subscribe);
    let writing = tokio::spawn(async move {
        while let Some(frame) = frames.recv().await {
            if sink.send(Message::Text(frame.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    shared.lock().writer = Some(writer);
    shared.set_state(HostState::Ready);
    while let Some(frame) = next_frame(&mut stream).await {
        match frame {
            Frame::Reply { id, res } => {
                if let Some(waiting) = shared.lock().waiting.remove(&id) {
                    let _ = waiting.send(res);
                }
            }
            Frame::Subscribed { complete, .. } => (shared.sink)(Incoming::Subscribed {
                complete: complete && same_boot,
            }),
            Frame::Event { seq, ev } => {
                shared.lock().since = Some((boot.clone(), seq));
                (shared.sink)(Incoming::Event { seq, event: ev });
            }
            Frame::Ping => {
                let pong = serde_json::to_string(&Frame::Pong).expect("a frame");
                if let Some(writer) = &shared.lock().writer {
                    let _ = writer.send(pong);
                }
            }
            _ => {}
        }
    }
    writing.abort();
    Ended::Lost { served: true }
}

fn text(frame: &Frame) -> Message {
    Message::Text(serde_json::to_string(frame).expect("a frame").into())
}

/// The next frame, or none once the link is gone. Text that isn't a frame
/// this client knows, from a newer daemon, is passed over.
async fn next_frame<S>(stream: &mut S) -> Option<Frame>
where
    S: Stream<Item = Result<Message, tungstenite::Error>> + Unpin,
{
    loop {
        match stream.next().await? {
            Ok(Message::Text(text)) => match serde_json::from_str(&text) {
                Ok(frame) => return Some(frame),
                Err(_) => continue,
            },
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => continue,
        }
    }
}

#[cfg(test)]
mod tests;

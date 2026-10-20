//! One host's connection (4.1): the WebSocket that carries the calls and
//! the events, HTTP for what is too big for a frame and for files and
//! packs, and a new connection whenever the link drops, waiting longer
//! each time (6.10).

use crate::connector::{Connector, Refusal};
use crate::{http, packs};
use futures_util::{SinkExt, Stream, StreamExt};
use git2::Oid;
use rovik_protocol::packs::Pushed;
use rovik_protocol::sync::{ChatBundle, Imported};
use rovik_protocol::{
    features, Event, Fail, FailCode, Frame, Request, Response, Secret, MAX_BUNDLE, MAX_CALL,
    MAX_FRAME, PROTOCOL,
};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::tungstenite::{self, ClientRequestBuilder, Message};

/// How long a call waits for its reply, unless it says otherwise.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a call over HTTP (`POST /call`) may take: a message of
/// megabytes (attached text files) on a slow uplink, or a reply of that
/// size, which a frame's 60 s would cut, and the daemon then run while
/// the app gives the message back to the input (6.11).
pub const POST_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// A session up this long served: lost after it, the next connection is
/// tried at once, not after a backoff.
const SERVED_AWHILE: Duration = Duration::from_secs(30);
/// How long the daemon has to answer `Hello`.
const WELCOME_WAIT: Duration = Duration::from_secs(10);
/// The daemon's answer to a token it doesn't take.
const TOKEN_REFUSED: &str = "The daemon doesn't know this computer's token: deploy again.";
/// After a lost link, the client waits this long before connecting again,
/// twice as long after each failed try, up to `MAX_BACKOFF` (6.10).
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// The wall clock this far ahead of the monotonic one, which stops while
/// the computer sleeps: it slept, and its link likely died meanwhile, so
/// the client connects again at once (6.10). Looked at every `CLOCK_TICK`.
const SLEPT: Duration = Duration::from_secs(30);
const CLOCK_TICK: Duration = Duration::from_secs(5);
/// A refusal that may come right on its own (`Refusal::may_pass`) is
/// tried again this many minutes after, quietly.
const REFUSED_AGAIN_MINUTES: u64 = 10;
#[cfg(not(test))]
const REFUSED_AGAIN: Duration = Duration::from_secs(REFUSED_AGAIN_MINUTES * 60);
#[cfg(test)]
const REFUSED_AGAIN: Duration = Duration::from_millis(300);

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
    /// The daemon serves only apps newer than this one (4.5). The client
    /// asks again now and then, quietly, at the slowest pace.
    VersionMismatch,
    /// The host turned this computer away: the credentials, its key,
    /// Rovik's install there, this computer's token. `Client::refusal`
    /// says why. What only the user can change (the credentials, a key to
    /// pin, the token) isn't tried again: the host's settings, a deploy or
    /// a test that gets through connect it anew. What may come right on
    /// its own (Rovik's install, the host's pinned key back) is tried again
    /// every `REFUSED_AGAIN_MINUTES`, quietly, Refused meanwhile.
    Refused,
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
// Nearly all are events, each handed on as it comes: boxing them would
// cost an allocation each and save nothing.
#[allow(clippy::large_enum_variant)]
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
    /// Dropped with the last clone, which ends the connection's task; or
    /// told to end it (`stop`), for a client put in place of this one.
    alive: Arc<watch::Sender<bool>>,
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
    /// Why the host turned this computer away (`HostState::Refused`).
    refusal: Option<String>,
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
        let (alive, stop) = watch::channel(false);
        tokio::spawn(run(shared.clone(), stop));
        Client {
            shared,
            alive: Arc::new(alive),
        }
    }

    /// Ends the connection and its task, whatever clones are still held
    /// (a call waiting on it fails as the link drops): for a client another
    /// takes the place of.
    pub fn stop(&self) {
        let _ = self.alive.send(true);
    }

    pub fn state(&self) -> HostState {
        self.shared.lock().state
    }

    /// Why the host turned this computer away, while it does
    /// (`HostState::Refused`).
    pub fn refusal(&self) -> Option<String> {
        self.shared.lock().refusal.clone()
    }

    /// What the user calls the host: its settings' name (`Identity::host`).
    pub fn name(&self) -> String {
        self.shared.identity.host.clone()
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
        let late = || {
            self.shared.lock().waiting.remove(&id);
            Fail::new(
                FailCode::Unreachable,
                format!("{host} didn't answer in time."),
            )
        };
        // A call over HTTP carries a big message, or brings a big reply, on
        // a link that may be slow: it gets `POST_TIMEOUT`, not the frame's.
        let posted = |call: String| async {
            tokio::time::timeout(POST_TIMEOUT, self.post_call(call))
                .await
                .unwrap_or_else(|_| Err(late()))
        };
        if call.len() > MAX_FRAME {
            return posted(call).await;
        }
        match tokio::time::timeout(timeout, self.over_websocket(id, call.clone())).await {
            // Only a read's reply is that big, so asking again is safe.
            Ok(Err(fail)) if fail.code == FailCode::TooLarge => posted(call).await,
            Ok(reply) => reply,
            Err(_) => Err(late()),
        }
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

    /// Sends the bundle that moves a chat to the daemon (6.8). Sent again
    /// with the same `move_id`, it's imported once. `progress` hears the
    /// bytes sent so far.
    pub async fn import_chat(
        &self,
        move_id: &str,
        bundle: &ChatBundle,
        progress: impl Fn(u64) + Send + Sync + 'static,
    ) -> Result<Imported, Fail> {
        self.takes_moves()?;
        let bytes =
            serde_json::to_vec(bundle).map_err(|e| Fail::new(FailCode::Internal, e.to_string()))?;
        if bytes.len() > MAX_BUNDLE {
            return Err(Fail::new(
                FailCode::TooLarge,
                format!(
                    "This chat is too big to move to {}: {} MB, {} MB at most.",
                    self.shared.identity.host,
                    bytes.len().div_ceil(1 << 20),
                    MAX_BUNDLE >> 20
                ),
            ));
        }
        let (connector, identity) = (&*self.shared.connector, &self.shared.identity);
        http::import_chat(connector, identity, move_id, bytes, Box::new(progress)).await
    }

    /// Sends an attachment's file (6.11), telling `progress` the bytes sent
    /// so far.
    pub async fn put_blob(
        &self,
        id: &str,
        bytes: Vec<u8>,
        progress: impl Fn(u64) + Send + Sync + 'static,
    ) -> Result<(), Fail> {
        let (connector, identity) = (&*self.shared.connector, &self.shared.identity);
        http::put_blob(connector, identity, id, bytes, Box::new(progress)).await
    }

    /// An attachment's file from the daemon, such as a screenshot a run
    /// took there; none when it has none.
    pub async fn blob(&self, id: &str) -> Result<Option<Vec<u8>>, Fail> {
        let (connector, identity) = (&*self.shared.connector, &self.shared.identity);
        let bytes = http::get_blob(connector, identity, id).await?;
        Ok(bytes.map(|b| b.to_vec()))
    }

    /// Sends `new` from the shadow repository at `shadow` to the project's
    /// ref `name` on the daemon (6.4), telling `progress` the bytes sent so
    /// far.
    pub async fn push(
        &self,
        project: &str,
        shadow: &Path,
        name: &str,
        new: Oid,
        progress: impl Fn(u64) + Send + Sync + 'static,
    ) -> Result<Pushed, Fail> {
        self.takes_projects()?;
        let (connector, identity) = (&*self.shared.connector, &self.shared.identity);
        let progress = Box::new(progress);
        packs::push(connector, identity, shadow, project, name, new, progress).await
    }

    /// Takes what the daemon has of `wants` for the project into the
    /// shadow repository at `shadow` (6.4). Returns how many objects came.
    pub async fn fetch(&self, project: &str, shadow: &Path, wants: &[Oid]) -> Result<usize, Fail> {
        self.takes_projects()?;
        let (connector, identity) = (&*self.shared.connector, &self.shared.identity);
        packs::fetch(connector, identity, shadow, project, wants).await
    }

    /// A daemon from before the mirror can't take a chat (4.5).
    fn takes_moves(&self) -> Result<(), Fail> {
        match self.daemon() {
            Some(daemon) if !daemon.features.iter().any(|f| f == features::MIRROR) => {
                Err(Fail::new(
                    FailCode::Version,
                    "The server's daemon can't take chats yet: it needs an upgrade.",
                ))
            }
            _ => Ok(()),
        }
    }

    /// A daemon from before the project transport has no `/packs` (4.5).
    fn takes_projects(&self) -> Result<(), Fail> {
        match self.daemon() {
            Some(daemon) if !daemon.features.iter().any(|f| f == features::PROJECTS) => {
                Err(Fail::new(
                    FailCode::Version,
                    "The server's daemon can't take projects yet: it needs an upgrade.",
                ))
            }
            _ => Ok(()),
        }
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

    /// The connection's state. Out of `Refused`, its refusal goes.
    fn set_state(&self, state: HostState) {
        let before = {
            let mut link = self.lock();
            if state != HostState::Refused {
                link.refusal = None;
            }
            std::mem::replace(&mut link.state, state)
        };
        if before != state {
            (self.sink)(Incoming::State(state));
        }
    }

    /// The host turned this computer away, for `why`: told again when why
    /// changed, as on a later try.
    fn refused(&self, why: String) {
        let told = {
            let mut link = self.lock();
            let same = link.state == HostState::Refused && link.refusal.as_ref() == Some(&why);
            link.refusal = Some(why);
            link.state = HostState::Refused;
            same
        };
        if !told {
            (self.sink)(Incoming::State(HostState::Refused));
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
    TooOld,
    /// The host turned this computer away, and why (`Refusal`).
    Refused { why: String, may_pass: bool },
    /// The link dropped, or never came up; `served` when it was ready.
    Lost { served: bool },
    /// The computer slept.
    Slept,
}

/// Sessions, one after another, until the client goes: its last clone
/// dropped, or `stop` told.
async fn run(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let mut wait = FIRST_BACKOFF;
    loop {
        // A daemon that serves only newer apps, or a host that turned
        // this computer away, is asked again without showing Connecting
        // each time: it's the same answer until something changes.
        let quiet = [HostState::VersionMismatch, HostState::Refused];
        if !quiet.contains(&shared.lock().state) {
            shared.set_state(HostState::Connecting);
        }
        let ended = tokio::select! {
            ended = session(&shared) => ended,
            _ = slept(CLOCK_TICK, SystemTime::now) => Ended::Slept,
            _ = stop.changed() => break,
        };
        shared.end_session();
        match ended {
            Ended::TooOld => {
                shared.set_state(HostState::VersionMismatch);
                wait = MAX_BACKOFF;
            }
            Ended::Refused { why, may_pass } => {
                if !may_pass {
                    shared.refused(why);
                    // Not again: it would be turned away the same way,
                    // after the credentials went once more. The client
                    // waits to be let go of, or put in place of
                    // (`Hosts::connect`).
                    let _ = stop.changed().await;
                    break;
                }
                shared.refused(format!(
                    "{why} Rovik tries again every {REFUSED_AGAIN_MINUTES} minutes."
                ));
                wait = REFUSED_AGAIN;
            }
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
            _ = stop.changed() => break,
        }
        wait = (wait * 2).min(MAX_BACKOFF);
    }
    shared.end_session();
}

/// A task that ends with the session it serves, however the session ends:
/// dropped when the session's future is (the computer slept, the client
/// stopped), not only when it returns.
struct Ending(tokio::task::JoinHandle<()>);

impl Drop for Ending {
    fn drop(&mut self) {
        self.0.abort();
    }
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
    let io = match shared.connector.connect().await {
        Ok(io) => io,
        Err(e) => {
            return match Refusal::of(&e) {
                Some(refusal) => Ended::Refused {
                    why: refusal.why.clone(),
                    may_pass: refusal.may_pass,
                },
                None => lost,
            }
        }
    };
    let uri: Uri = "ws://rovik-agentd/rpc".parse().expect("a URI");
    let request = ClientRequestBuilder::new(uri)
        .with_header("Authorization", format!("Bearer {}", identity.token.0));
    let mut socket = match tokio_tungstenite::client_async(request, io).await {
        Ok((socket, _)) => socket,
        Err(tungstenite::Error::Http(response)) if response.status() == 401 => {
            return Ended::Refused {
                why: TOKEN_REFUSED.into(),
                may_pass: false,
            }
        }
        Err(_) => return lost,
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
        return Ended::TooOld;
    }

    let (since, same_boot) = shared.resume_from(&boot);
    let (mut sink, mut stream) = socket.split();
    let (writer, mut frames) = mpsc::unbounded_channel::<String>();
    let subscribe = serde_json::to_string(&Frame::Subscribe { since }).expect("a frame");
    let _ = writer.send(subscribe);
    let _writing = Ending(tokio::spawn(async move {
        while let Some(frame) = frames.recv().await {
            if sink.send(Message::Text(frame.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    }));
    shared.lock().writer = Some(writer);
    let served_since = std::time::Instant::now();
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
    // Served: it stayed up a while. A daemon that drops the link right
    // after Welcome (one in a crash loop) isn't reconnected at once, every
    // second, for ever: the backoff goes on as for a failed connection.
    Ended::Lost {
        served: served_since.elapsed() >= SERVED_AWHILE,
    }
}

/// Says Hello to the daemon once, and returns its Welcome (H05): that it
/// takes this computer's token, and what it is.
pub async fn welcome(connector: &dyn Connector, identity: &Identity) -> Result<DaemonInfo, Fail> {
    let lost = |why: String| Fail::new(FailCode::Unreachable, why);
    let io = connector.connect().await.map_err(|e| lost(e.to_string()))?;
    let uri: Uri = "ws://rovik-agentd/rpc".parse().expect("a URI");
    let request = ClientRequestBuilder::new(uri)
        .with_header("Authorization", format!("Bearer {}", identity.token.0));
    let mut socket = match tokio_tungstenite::client_async(request, io).await {
        Ok((socket, _)) => socket,
        Err(tungstenite::Error::Http(response)) if response.status() == 401 => {
            return Err(Fail::new(FailCode::BadRequest, TOKEN_REFUSED))
        }
        Err(e) => return Err(lost(e.to_string())),
    };
    let hello = Frame::Hello {
        protocol: PROTOCOL,
        client: identity.client.clone(),
        device: identity.computer.clone(),
        token: identity.token.clone(),
    };
    socket
        .send(text(&hello))
        .await
        .map_err(|e| lost(e.to_string()))?;
    let welcome = tokio::time::timeout(WELCOME_WAIT, next_frame(&mut socket)).await;
    let _ = socket.close(None).await;
    match welcome {
        Ok(Some(Frame::Welcome {
            protocol,
            min_protocol,
            daemon,
            features,
            ..
        })) => Ok(DaemonInfo {
            protocol,
            min_protocol,
            daemon,
            features,
        }),
        _ => Err(lost("The daemon didn't answer Hello.".into())),
    }
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

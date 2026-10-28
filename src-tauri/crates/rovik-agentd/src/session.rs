//! One WebSocket (4.1): the client's `Hello`, then its calls, pings and
//! `Subscribe`, each a JSON text frame of at most `MAX_FRAME` bytes. Calls
//! are answered side by side, so a slow one holds up neither the others
//! nor the pings, and one writer sends every frame out: replies, and the
//! events of this computer's chats once it subscribes (4.3).

use crate::cli::VERSION;
use crate::daemon::{Daemon, MIN_PROTOCOL};
use crate::events::{Events, Published};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use rovik_protocol::{features, Fail, FailCode, Frame, MAX_FRAME, PROTOCOL};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// How long a new connection has to say `Hello`.
const HELLO_WAIT: Duration = Duration::from_secs(10);
/// How long a frame may take to go out: a client that stops reading (a
/// computer gone to sleep mid-connection) is let go, not waited on.
const WRITE_WAIT: Duration = Duration::from_secs(30);
/// What this daemon serves beyond protocol 1's start (4.5).
const FEATURES: &[&str] = &[
    features::ATTACHMENTS,
    features::PROJECTS,
    features::MIRROR,
    features::HOST,
    features::CLEAN_UP,
    features::AUTONOMY,
    features::MCP,
];
/// Frames waiting for the writer. A connection whose events back up past
/// this falls behind the live ones and is dropped.
const QUEUE: usize = 256;

type Sink<S> = SplitSink<WebSocketStream<S>, Message>;
type Stream<S> = SplitStream<WebSocketStream<S>>;

/// What the writer sends.
enum Out {
    /// Boxed: a frame is far bigger than an event's text, and the queue
    /// holds mostly events.
    Frame(Box<Frame>),
    /// An event, already a frame's text.
    Event(Arc<Published>),
    /// The connection fell behind: close it, so the client subscribes again.
    Behind,
}

pub async fn run<S>(daemon: Arc<Daemon>, computer: String, io: S)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME));
    let socket = WebSocketStream::from_raw_socket(io, Role::Server, Some(config)).await;
    let (mut sink, mut stream) = socket.split();

    // Hello first, with the same token as the upgrade.
    let hello = tokio::time::timeout(HELLO_WAIT, read(&mut stream)).await;
    let (protocol, client) = match hello {
        Ok(Some(Incoming::Frame(Frame::Hello {
            protocol,
            client,
            device,
            token,
        }))) if device == computer && daemon.tokens.computer(&token.0) == Some(&computer) => {
            (protocol, client)
        }
        Ok(Some(Incoming::Frame(Frame::Hello { .. }))) => {
            return close(
                &mut sink,
                CloseCode::Policy,
                "The token doesn't name this computer.",
            )
            .await;
        }
        _ => {
            return close(
                &mut sink,
                CloseCode::Protocol,
                "A session starts with Hello.",
            )
            .await
        }
    };
    let welcome = Out::Frame(Box::new(Frame::Welcome {
        protocol: PROTOCOL,
        min_protocol: MIN_PROTOCOL,
        daemon: format!("rovik-agentd {VERSION}"),
        boot: daemon.events.boot.clone(),
        features: FEATURES.iter().map(|f| f.to_string()).collect(),
    }));
    if send(&mut sink, &welcome).await.is_err() {
        return;
    }
    if protocol < MIN_PROTOCOL {
        // The Welcome says why: the client compares the protocols.
        return close(
            &mut sink,
            CloseCode::Policy,
            "The app is older than this daemon serves.",
        )
        .await;
    }
    log::info!(
        "{computer} connected ({}, protocol {protocol})",
        plain(&client)
    );

    let (tx, rx) = mpsc::channel::<Out>(QUEUE);
    let mut writer = tokio::spawn(write_out(sink, rx, WRITE_WAIT));
    let mut writer_ended = false;
    let mut forwarder: Option<JoinHandle<()>> = None;
    loop {
        // A writer that let the client go ends the session too.
        let incoming = tokio::select! {
            incoming = read(&mut stream) => incoming,
            _ = &mut writer => {
                writer_ended = true;
                None
            }
        };
        let Some(incoming) = incoming else {
            break;
        };
        match incoming {
            Incoming::Frame(Frame::Call { id, req }) => {
                let (daemon, tx, computer) = (daemon.clone(), tx.clone(), computer.clone());
                tokio::spawn(async move {
                    // A call that panics is still answered: its task alone
                    // ends, and the client would otherwise wait for a reply
                    // that never comes.
                    let answered =
                        tokio::spawn(async move { daemon.answer(&computer, req).await }).await;
                    let res = answered.unwrap_or_else(|e| {
                        log::error!("a call failed on the server: {e}");
                        Err(Fail::new(
                            FailCode::Internal,
                            "The request failed on the server; its log says why.",
                        ))
                    });
                    let reply = Frame::Reply { id, res };
                    let _ = tx.send(Out::Frame(Box::new(reply))).await;
                });
            }
            Incoming::Frame(Frame::Subscribe { since }) => {
                if let Some(old) = forwarder.take() {
                    old.abort();
                }
                forwarder = Some(subscribe(&daemon.events, &computer, since, tx.clone()));
            }
            Incoming::Frame(Frame::Ping) => {
                let _ = tx.send(Out::Frame(Box::new(Frame::Pong))).await;
            }
            Incoming::Frame(Frame::Pong) => {}
            // A request this daemon doesn't know, from a newer client.
            Incoming::Unknown { id: Some(id) } => {
                let fail = Fail::new(
                    FailCode::BadRequest,
                    "This daemon doesn't know the request.",
                );
                let reply = Frame::Reply { id, res: Err(fail) };
                let _ = tx.send(Out::Frame(Box::new(reply))).await;
            }
            Incoming::Frame(_) | Incoming::Unknown { id: None } => {
                log::debug!("{computer} sent a frame the daemon doesn't take: ignored");
            }
        }
    }
    if let Some(forwarder) = forwarder {
        forwarder.abort();
    }
    drop(tx);
    if !writer_ended {
        let _ = writer.await;
    }
    log::info!("{computer} disconnected");
}

/// Sends what's queued, one frame at a time, until the queue closes; a
/// frame the client takes none of for `wait` lets it go (it stopped
/// reading), as one that fell behind is.
async fn write_out<S>(mut sink: Sink<S>, mut rx: mpsc::Receiver<Out>, wait: Duration)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    while let Some(out) = rx.recv().await {
        if matches!(out, Out::Behind) {
            return close(&mut sink, CloseCode::Again, "Fell behind: subscribe again.").await;
        }
        match tokio::time::timeout(wait, send(&mut sink, &out)).await {
            Ok(Ok(())) => {}
            Ok(Err(())) => break,
            Err(_) => {
                log::info!("a client took no frame for {} s: let go", wait.as_secs());
                return;
            }
        }
    }
    let _ = sink.close().await;
}

/// A task that sends `Subscribed`, the events the client missed, then its
/// computer's events as they come. The subscription starts now; what it
/// sends waits for the writer in the task, never in the session's reading.
fn subscribe(
    events: &Events,
    computer: &str,
    since: Option<(String, u64)>,
    tx: mpsc::Sender<Out>,
) -> JoinHandle<()> {
    let mut subscription = events.subscribe(computer, since);
    let subscribed = Frame::Subscribed {
        replayed: subscription.replay.len() as u64,
        complete: subscription.complete,
    };
    let computer = computer.to_string();
    tokio::spawn(async move {
        if tx.send(Out::Frame(Box::new(subscribed))).await.is_err() {
            return;
        }
        for published in subscription.replay.drain(..) {
            if tx.send(Out::Event(published)).await.is_err() {
                return;
            }
        }
        loop {
            match subscription.live.recv().await {
                Ok(published) if published.reaches(&computer) => {
                    if tx.send(Out::Event(published)).await.is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(missed)) => {
                    log::info!("{computer} fell {missed} events behind: dropped");
                    let _ = tx.send(Out::Behind).await;
                    break;
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}

/// What a client says of itself, fit for one line of the log: control
/// characters out, and no longer than a line.
fn plain(client: &str) -> String {
    let mut text: String = client
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if text.chars().count() > 80 {
        text = text.chars().take(79).chain(std::iter::once('…')).collect();
    }
    text
}

// Each is matched as soon as it's read, and nearly all are frames: boxing
// them would cost an allocation each and save nothing.
#[allow(clippy::large_enum_variant)]
enum Incoming {
    Frame(Frame),
    /// Text that isn't a frame this daemon knows; a call's id if it has one.
    Unknown {
        id: Option<u64>,
    },
}

/// The next frame, or none when the client is gone.
async fn read<S>(stream: &mut Stream<S>) -> Option<Incoming>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        match stream.next().await? {
            Ok(Message::Text(text)) => {
                return Some(match serde_json::from_str::<Frame>(&text) {
                    Ok(frame) => Incoming::Frame(frame),
                    Err(_) => Incoming::Unknown { id: call_id(&text) },
                });
            }
            Ok(Message::Close(_)) => return None,
            Ok(_) => continue,
            Err(e) => {
                log::debug!("a WebSocket ended: {e}");
                return None;
            }
        }
    }
}

fn call_id(text: &str) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    (value["t"] == "call").then(|| value["id"].as_u64())?
}

/// Sends a frame. A reply too big for a frame goes as `TooLarge` instead:
/// the client asks again through `POST /call` (4.1). Events are sized
/// before they get here.
async fn send<S>(sink: &mut Sink<S>, out: &Out) -> Result<(), ()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let text = match out {
        Out::Event(published) => published.text.to_string(),
        Out::Frame(frame) => frame_text(frame).ok_or(())?,
        Out::Behind => return Ok(()),
    };
    sink.send(Message::Text(text.into())).await.map_err(|_| ())
}

fn frame_text(frame: &Frame) -> Option<String> {
    let text = serde_json::to_string(frame).ok()?;
    match frame {
        Frame::Reply { id, .. } if text.len() > MAX_FRAME => {
            let fail = Fail::new(
                FailCode::TooLarge,
                format!(
                    "The reply is {} KB, over the {} KB a frame takes.",
                    text.len() >> 10,
                    MAX_FRAME >> 10
                ),
            );
            serde_json::to_string(&Frame::Reply {
                id: *id,
                res: Err(fail),
            })
            .ok()
        }
        _ => Some(text),
    }
}

async fn close<S>(sink: &mut Sink<S>, code: CloseCode, reason: &str)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let frame = CloseFrame {
        code,
        reason: reason.into(),
    };
    let _ = sink.send(Message::Close(Some(frame))).await;
    let _ = sink.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rovik_core::agent_run::events::AgentEvent;
    use rovik_core::EventSink;

    /// Subscribing doesn't wait for the replay to go out: a queue with
    /// room for one frame still gets `Subscribed`, then every missed event
    /// in order, as the writer takes them.
    #[tokio::test]
    async fn a_replay_waits_for_the_writer_in_its_own_task() {
        let owner: crate::events::Owner =
            Arc::new(|chat: &str| (chat == "chat").then(|| "laptop".to_string()));
        let events = Events::new("boot".into(), owner);
        for n in 0..5 {
            events.agent(&AgentEvent::RunStarted {
                dialog_id: "chat".into(),
                run_id: format!("r{n}"),
                steerable: false,
            });
        }
        let (tx, mut rx) = mpsc::channel(1);
        let forwarder = subscribe(&events, "laptop", Some(("boot".into(), 0)), tx);
        let Some(Out::Frame(first)) = rx.recv().await else {
            panic!("Subscribed first");
        };
        assert!(matches!(*first, Frame::Subscribed { replayed: 5, .. }));
        let mut seqs = Vec::new();
        for _ in 0..5 {
            match rx.recv().await {
                Some(Out::Event(published)) => seqs.push(published.seq),
                _ => panic!("an event"),
            }
        }
        assert_eq!(seqs, [1, 2, 3, 4, 5]);
        forwarder.abort();
    }

    /// A client that stops reading is let go once a frame has waited for
    /// it the write timeout, instead of holding its session for good.
    #[tokio::test]
    async fn a_client_that_stops_reading_is_let_go() {
        let (server, _client) = tokio::io::duplex(1024);
        let socket = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
        let (sink, _stream) = socket.split();
        let (tx, rx) = mpsc::channel(4);
        let big = Frame::Welcome {
            protocol: PROTOCOL,
            min_protocol: PROTOCOL,
            daemon: "x".repeat(64 << 10),
            boot: "boot".into(),
            features: Vec::new(),
        };
        tx.send(Out::Frame(Box::new(big))).await.unwrap();
        let writing = write_out(sink, rx, Duration::from_millis(200));
        let ended = tokio::time::timeout(Duration::from_secs(5), writing).await;
        assert!(ended.is_ok(), "the writer still waits on the client");
        drop(tx);
    }
}

//! One WebSocket (4.1): the client's `Hello`, then its calls, pings and
//! `Subscribe`, each a JSON text frame of at most `MAX_FRAME` bytes. Calls
//! are answered side by side, so a slow one holds up neither the others
//! nor the pings, and one writer sends every frame out: replies, and the
//! events of this computer's chats once it subscribes (4.3).

use crate::cli::VERSION;
use crate::daemon::{Daemon, MIN_PROTOCOL};
use crate::events::Published;
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
/// What this daemon serves beyond protocol 1's start (4.5).
const FEATURES: &[&str] = &[features::ATTACHMENTS, features::PROJECTS];
/// Frames waiting for the writer. A connection whose events back up past
/// this falls behind the live ones and is dropped.
const QUEUE: usize = 256;

type Sink<S> = SplitSink<WebSocketStream<S>, Message>;
type Stream<S> = SplitStream<WebSocketStream<S>>;

/// What the writer sends.
enum Out {
    Frame(Frame),
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
    let welcome = Frame::Welcome {
        protocol: PROTOCOL,
        min_protocol: MIN_PROTOCOL,
        daemon: format!("rovik-agentd {VERSION}"),
        boot: daemon.events.boot.clone(),
        features: FEATURES.iter().map(|f| f.to_string()).collect(),
    };
    if send(&mut sink, &Out::Frame(welcome)).await.is_err() {
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
    log::info!("{computer} connected ({client}, protocol {protocol})");

    let (tx, mut rx) = mpsc::channel::<Out>(QUEUE);
    let writer = tokio::spawn(async move {
        while let Some(out) = rx.recv().await {
            if matches!(out, Out::Behind) {
                return close(&mut sink, CloseCode::Again, "Fell behind: subscribe again.").await;
            }
            if send(&mut sink, &out).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let mut forwarder: Option<JoinHandle<()>> = None;
    while let Some(incoming) = read(&mut stream).await {
        match incoming {
            Incoming::Frame(Frame::Call { id, req }) => {
                let (daemon, tx, computer) = (daemon.clone(), tx.clone(), computer.clone());
                tokio::spawn(async move {
                    let res = daemon.answer(&computer, req).await;
                    let _ = tx.send(Out::Frame(Frame::Reply { id, res })).await;
                });
            }
            Incoming::Frame(Frame::Subscribe { since }) => {
                if let Some(old) = forwarder.take() {
                    old.abort();
                }
                forwarder = Some(subscribe(&daemon, &computer, since, tx.clone()).await);
            }
            Incoming::Frame(Frame::Ping) => {
                let _ = tx.send(Out::Frame(Frame::Pong)).await;
            }
            Incoming::Frame(Frame::Pong) => {}
            // A request this daemon doesn't know, from a newer client.
            Incoming::Unknown { id: Some(id) } => {
                let fail = Fail::new(
                    FailCode::BadRequest,
                    "This daemon doesn't know the request.",
                );
                let reply = Frame::Reply { id, res: Err(fail) };
                let _ = tx.send(Out::Frame(reply)).await;
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
    let _ = writer.await;
    log::info!("{computer} disconnected");
}

/// `Subscribed`, the events the client missed, then a task that sends it
/// its computer's events as they come.
async fn subscribe(
    daemon: &Daemon,
    computer: &str,
    since: Option<(String, u64)>,
    tx: mpsc::Sender<Out>,
) -> JoinHandle<()> {
    let mut subscription = daemon.events.subscribe(computer, since);
    let subscribed = Frame::Subscribed {
        replayed: subscription.replay.len() as u64,
        complete: subscription.complete,
    };
    let _ = tx.send(Out::Frame(subscribed)).await;
    for published in subscription.replay.drain(..) {
        let _ = tx.send(Out::Event(published)).await;
    }
    let computer = computer.to_string();
    tokio::spawn(async move {
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

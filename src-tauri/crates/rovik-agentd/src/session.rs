//! One WebSocket (4.1): the client's `Hello`, then its calls and pings,
//! each a JSON text frame of at most `MAX_FRAME` bytes. Calls are answered
//! side by side, so a slow one holds up neither the others nor the pings,
//! and one writer sends every frame out.

use crate::cli::VERSION;
use crate::daemon::{Daemon, MIN_PROTOCOL};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use rovik_protocol::{Fail, FailCode, Frame, MAX_FRAME, PROTOCOL};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// How long a new connection has to say `Hello`.
const HELLO_WAIT: Duration = Duration::from_secs(10);
/// Frames waiting for the writer.
const QUEUE: usize = 256;

type Sink<S> = SplitSink<WebSocketStream<S>, Message>;
type Stream<S> = SplitStream<WebSocketStream<S>>;

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
        boot: daemon.boot.clone(),
        features: Vec::new(),
    };
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
    log::info!("{computer} connected ({client}, protocol {protocol})");

    let (tx, mut rx) = mpsc::channel::<Frame>(QUEUE);
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if send(&mut sink, &frame).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    while let Some(incoming) = read(&mut stream).await {
        match incoming {
            Incoming::Frame(Frame::Call { id, req }) => {
                let (daemon, tx) = (daemon.clone(), tx.clone());
                tokio::spawn(async move {
                    let res = daemon.answer(req).await;
                    let _ = tx.send(Frame::Reply { id, res }).await;
                });
            }
            Incoming::Frame(Frame::Ping) => {
                let _ = tx.send(Frame::Pong).await;
            }
            Incoming::Frame(Frame::Pong) => {}
            // A request this daemon doesn't know, from a newer client.
            Incoming::Unknown { id: Some(id) } => {
                let fail = Fail::new(
                    FailCode::BadRequest,
                    "This daemon doesn't know the request.",
                );
                let _ = tx.send(Frame::Reply { id, res: Err(fail) }).await;
            }
            Incoming::Frame(_) | Incoming::Unknown { id: None } => {
                log::debug!("{computer} sent a frame the daemon doesn't take: ignored");
            }
        }
    }
    drop(tx);
    let _ = writer.await;
    log::info!("{computer} disconnected");
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
/// the client asks again through `POST /call` (4.1).
async fn send<S>(sink: &mut Sink<S>, frame: &Frame) -> Result<(), ()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut text = serde_json::to_string(frame).map_err(|_| ())?;
    if text.len() > MAX_FRAME {
        if let Frame::Reply { id, .. } = frame {
            let fail = Fail::new(
                FailCode::TooLarge,
                format!(
                    "The reply is {} KB, over the {} KB a frame takes.",
                    text.len() >> 10,
                    MAX_FRAME >> 10
                ),
            );
            text = serde_json::to_string(&Frame::Reply {
                id: *id,
                res: Err(fail),
            })
            .map_err(|_| ())?;
        }
    }
    sink.send(Message::Text(text.into())).await.map_err(|_| ())
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

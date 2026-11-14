//! Bounded typed RPC over the authenticated local stream. Calls are never
//! automatically retried: a lost reply may follow a committed mutation.

use super::{connect, Ready, Stream};
use crate::desktop_api::DesktopRequest;
use crate::desktop_backend::DesktopBackend;
use crate::desktop_session::{DesktopEvent, SessionEvents};
#[cfg(test)]
use rovik_protocol::Frame;
use rovik_protocol::{Fail, Request, Response};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{io, path::Path, sync::Arc, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

mod client;
mod server;
pub use client::Client;
pub use server::serve;

// The desktop accepts images up to 50 MiB, encoded as base64 on this wire.
// Keep the frame bounded while preserving the desktop attachment limit.
const MAX_MESSAGE: usize = {
    let attachment = rovik_core::chat_db::attachments::MAX_IMAGE_BYTES.div_ceil(3) * 4 + 4096;
    if attachment > rovik_protocol::MAX_CALL {
        attachment
    } else {
        rovik_protocol::MAX_CALL
    }
};
const IO_WAIT: Duration = Duration::from_secs(60);
// Cancellation controls keep capacity even while every ordinary call is waiting.
const ORDINARY_CALLS: usize = 32;
const CONTROL_CALLS: usize = 4;

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
enum ClientFrame {
    Call {
        id: u64,
        req: Box<Request>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host: Option<String>,
    },
    Desktop {
        id: u64,
        req: Box<DesktopRequest>,
    },
    Subscribe {
        session: String,
    },
    CloseSession {
        session: String,
    },
    CancelSink {
        sink: String,
    },
    Ping,
}

impl ClientFrame {
    fn is_control(&self) -> bool {
        use crate::desktop_api::{agent::AgentRequest, chat::ChatRequest};
        use rovik_protocol::harness::Operation;
        let harness_control = |operation: &Operation| {
            matches!(
                operation,
                Operation::Cancel { .. } | Operation::CancelPreparation { .. }
            )
        };
        match self {
            Self::Call { req, .. } => match req.as_ref() {
                Request::AgentStop { .. } | Request::BackgroundStop { .. } => true,
                Request::Harness { operation, .. } => harness_control(operation),
                _ => false,
            },
            Self::Desktop { req, .. } => match req.as_ref() {
                DesktopRequest::Agent(request) => {
                    matches!(request.as_ref(), AgentRequest::Cancel { .. })
                }
                DesktopRequest::Chat(request) => {
                    matches!(request.as_ref(), ChatRequest::CancelMove { .. })
                }
                DesktopRequest::Harness(request) => harness_control(&request.request),
                _ => false,
            },
            _ => false,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
enum ServerFrame {
    Reply {
        id: u64,
        res: Box<Result<Response, Fail>>,
    },
    DesktopReply {
        id: u64,
        res: Result<serde_json::Value, serde_json::Value>,
    },
    Subscribed {
        result: Result<(), String>,
    },
    Event {
        event: DesktopEvent,
    },
    EventFailure {
        message: String,
    },
    SessionClosed,
    Pong,
}

async fn read<T: DeserializeOwned>(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<T> {
    let size = stream.read_u32().await? as usize;
    read_body(stream, size).await
}

// Readiness can be stale after the previous frame (notably with kqueue).
// Only an actual byte starts the deadline; a quiet window is not a stalled sender.
async fn read_after_idle<T: DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
    wait: Duration,
) -> io::Result<T> {
    let first = stream.read_u8().await?;
    tokio::time::timeout(wait, async {
        let mut header = [first, 0, 0, 0];
        stream.read_exact(&mut header[1..]).await?;
        read_body(stream, u32::from_be_bytes(header) as usize).await
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "Local RPC sender did not finish its frame",
        )
    })?
}

async fn read_body<T: DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
    size: usize,
) -> io::Result<T> {
    if size == 0 || size > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid local RPC message size",
        ));
    }
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Malformed local RPC message at line {}, column {}",
                e.line(),
                e.column()
            ),
        )
    })
}

async fn write(stream: &mut (impl AsyncWrite + Unpin), value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Local RPC message exceeds its size limit",
        ));
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await
}

async fn write_reply(
    stream: &mut (impl AsyncWrite + Unpin),
    value: &impl Serialize,
) -> io::Result<()> {
    tokio::time::timeout(IO_WAIT, write(stream, value))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Local RPC peer stopped reading"))?
}

/// The private event connection of one desktop client.
pub struct EventStream {
    stream: Stream,
}

impl EventStream {
    /// Stops the producer when its native UI callback disappears.
    pub async fn reject(&mut self, sink: String) -> Result<(), String> {
        tokio::time::timeout(
            IO_WAIT,
            write(&mut self.stream, &ClientFrame::CancelSink { sink }),
        )
        .await
        .map_err(|_| "Desktop event cancellation timed out".to_string())?
        .map_err(|e| e.to_string())
    }

    /// Reads in source order. Loss is explicit and requires a new client session.
    pub async fn next(&mut self) -> Result<DesktopEvent, String> {
        match read(&mut self.stream)
            .await
            .map_err(|e| format!("Desktop event connection failed: {e}"))?
        {
            ServerFrame::Event { event } => Ok(event),
            ServerFrame::EventFailure { message } => Err(message),
            _ => Err("Unexpected desktop event frame".into()),
        }
    }
}

async fn serve_events(stream: Stream, mut events: SessionEvents) -> io::Result<()> {
    let (mut input, mut output) = tokio::io::split(stream);
    let session = events.session();
    // Read complete cancellation frames independently: selecting directly on
    // read() would discard partially read frames whenever an event arrived.
    let mut reader = tokio::spawn(async move {
        loop {
            match read_after_idle(&mut input, IO_WAIT).await? {
                ClientFrame::CancelSink { sink } => session.cancel_sink(&sink),
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Unexpected event-stream input",
                    ))
                }
            }
        }
    });
    struct Abort(tokio::task::AbortHandle);
    impl Drop for Abort {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _abort = Abort(reader.abort_handle());
    loop {
        let event = tokio::select! {
            event = events.next() => event,
            result = &mut reader => return match result {
                Ok(Err(error)) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(()),
                Ok(result) => result,
                Err(error) => Err(io::Error::other(error)),
            },
        };
        let (frame, ended) = match event {
            Ok(event) => (ServerFrame::Event { event }, false),
            Err(message) => (ServerFrame::EventFailure { message }, true),
        };
        tokio::time::timeout(IO_WAIT, write(&mut output, &frame))
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Desktop event consumer stopped reading",
                )
            })??;
        if ended {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) async fn stream_pair() -> (Stream, Stream) {
        let root = tempfile::tempdir().unwrap();
        let endpoint = super::super::platform::Endpoint::bind(root.path(), "rpc").unwrap();
        let (server, client) = tokio::join!(
            endpoint.accept(),
            super::super::platform::connect_endpoint(root.path(), "rpc")
        );
        (client.unwrap(), server.unwrap())
    }

    #[tokio::test]
    async fn idle_between_frames_does_not_start_the_frame_deadline() {
        let (mut sender, mut receiver) = stream_pair().await;
        let wait = Duration::from_millis(10);
        write(&mut sender, &ClientFrame::Ping).await.unwrap();
        assert!(matches!(
            read_after_idle::<ClientFrame>(&mut receiver, wait)
                .await
                .unwrap(),
            ClientFrame::Ping
        ));
        let next = read_after_idle::<ClientFrame>(&mut receiver, wait);
        tokio::pin!(next);
        assert!(tokio::time::timeout(wait * 3, &mut next).await.is_err());
        write(&mut sender, &ClientFrame::Ping).await.unwrap();
        assert!(matches!(next.await.unwrap(), ClientFrame::Ping));
    }

    #[tokio::test]
    async fn a_started_header_or_body_must_finish_before_the_deadline() {
        let payload = serde_json::to_vec(&ClientFrame::Ping).unwrap();
        let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
        frame.extend(payload);
        for prefix in [1, 4, 5] {
            let (mut sender, mut receiver) = stream_pair().await;
            sender.write_all(&frame[..prefix]).await.unwrap();
            let result =
                read_after_idle::<ClientFrame>(&mut receiver, Duration::from_millis(10)).await;
            assert_eq!(result.err().unwrap().kind(), io::ErrorKind::TimedOut);
        }
    }

    #[tokio::test]
    async fn detaching_interrupts_a_pending_reply_even_when_control_cannot_reconnect() {
        let (mut client, mut server) = pair().await;
        let directory = tempfile::tempdir().unwrap();
        client.directory = directory.path().to_path_buf();
        let (accepted, received) = tokio::sync::oneshot::channel();
        let peer = tokio::spawn(async move {
            assert!(matches!(
                read::<ClientFrame>(&mut server).await.unwrap(),
                ClientFrame::Desktop { .. }
            ));
            accepted.send(()).unwrap();
            // No reply: detach must drop this transport, never replay the call.
            assert_eq!(
                read::<ClientFrame>(&mut server).await.err().unwrap().kind(),
                io::ErrorKind::UnexpectedEof
            );
        });
        let call = client.desktop(crate::desktop_api::vfs::VfsRequest::Status.into());
        tokio::pin!(call);
        tokio::select! {
            _ = received => {},
            result = &mut call => panic!("call finished before detach: {result:?}"),
        }
        let closed = tokio::time::timeout(Duration::from_secs(2), client.close())
            .await
            .unwrap();
        assert!(
            closed.is_err(),
            "failure to acknowledge detach must be reported"
        );
        assert_eq!(call.await.unwrap_err().kind(), io::ErrorKind::NotConnected);
        assert!(client.call(Request::AgentStatus).await.is_err());
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn desktop_calls_reject_cloud_replies_and_do_not_reuse_the_connection() {
        let (client, mut server) = pair().await;
        let peer = tokio::spawn(async move {
            let frame: ClientFrame = read(&mut server).await.unwrap();
            assert!(matches!(frame, ClientFrame::Desktop { id: 1, .. }));
            write(
                &mut server,
                &ServerFrame::Reply {
                    id: 1,
                    res: Box::new(Err(Fail::new(
                        rovik_protocol::FailCode::BadRequest,
                        "wrong reply family",
                    ))),
                },
            )
            .await
            .unwrap();
        });
        let request = || DesktopRequest::from(crate::desktop_api::vfs::VfsRequest::Status);
        assert_eq!(
            client.desktop(request()).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        peer.await.unwrap();
        assert_eq!(
            client.desktop(request()).await.unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
    }

    pub(super) async fn pair() -> (Client, Stream) {
        let (client, server) = stream_pair().await;
        (
            Client::start(
                Path::new(""),
                client,
                Ready {
                    protocol: super::super::PROTOCOL,
                    boot: "test".into(),
                    version: "test".into(),
                    session: "test".into(),
                },
            ),
            server,
        )
    }

    #[tokio::test]
    async fn a_lost_reply_invalidates_the_connection_without_retrying() {
        let (client, mut server) = pair().await;
        let peer = tokio::spawn(async move {
            let request: Frame = read(&mut server).await.unwrap();
            assert!(matches!(
                request,
                Frame::Call {
                    req: Request::AgentStatus,
                    ..
                }
            ));
            // The request could have taken effect; no reply reaches its client.
        });
        assert!(client.call(Request::AgentStatus).await.is_err());
        peer.await.unwrap();
        assert_eq!(
            client.call(Request::AgentStatus).await.unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
    }

    #[tokio::test]
    async fn a_mismatched_reply_cannot_be_used_by_a_later_call() {
        let (client, mut server) = pair().await;
        let peer = tokio::spawn(async move {
            let _: Frame = read(&mut server).await.unwrap();
            write(
                &mut server,
                &Frame::Reply {
                    id: 2,
                    res: Ok(Response::Done),
                },
            )
            .await
            .unwrap();
        });
        assert_eq!(
            client.call(Request::AgentStatus).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        peer.await.unwrap();
        assert_eq!(
            client.call(Request::AgentStatus).await.unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
    }

    #[tokio::test]
    async fn cancelling_an_inflight_call_preserves_other_calls_and_discards_its_late_reply() {
        cancelled_reply(false).await;
    }

    #[tokio::test]
    async fn cancelling_a_desktop_operation_preserves_other_calls_and_discards_its_late_reply() {
        cancelled_reply(true).await;
    }

    async fn cancelled_reply(desktop: bool) {
        let (client, mut server) = pair().await;
        let client = Arc::new(client);
        let calling = client.clone();
        let task = tokio::spawn(async move {
            if desktop {
                calling
                    .desktop(crate::desktop_api::vfs::VfsRequest::Status.into())
                    .await
                    .map(|_| ())
            } else {
                calling.call(Request::AgentStatus).await.map(|_| ())
            }
        });
        let _: ClientFrame = read(&mut server).await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let calling = client.clone();
        let next = tokio::spawn(async move { calling.call(Request::AgentStatus).await });
        assert!(matches!(
            read::<ClientFrame>(&mut server).await.unwrap(),
            ClientFrame::Call { id: 2, .. }
        ));
        let late = if desktop {
            ServerFrame::DesktopReply {
                id: 1,
                res: Ok(serde_json::json!("late")),
            }
        } else {
            ServerFrame::Reply {
                id: 1,
                res: Box::new(Ok(Response::Done)),
            }
        };
        write(&mut server, &late).await.unwrap();
        write(
            &mut server,
            &ServerFrame::Reply {
                id: 2,
                res: Box::new(Err(Fail::new(
                    rovik_protocol::FailCode::Busy,
                    "second reply",
                ))),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            next.await.unwrap().unwrap().unwrap_err().message,
            "second reply"
        );
        assert!(client.usable());
    }

    #[tokio::test]
    async fn explicit_host_calls_preserve_their_destination() {
        let (client, mut server) = pair().await;
        let peer = tokio::spawn(async move {
            let call: ClientFrame = read(&mut server).await.unwrap();
            assert!(
                matches!(call, ClientFrame::Call { host: Some(host), req, .. } if host == "build-host" && *req == Request::HostInfo)
            );
            write(
                &mut server,
                &Frame::Reply {
                    id: 1,
                    res: Ok(Response::Done),
                },
            )
            .await
            .unwrap();
        });
        assert!(matches!(
            client
                .call_host("build-host", Request::HostInfo)
                .await
                .unwrap()
                .unwrap(),
            Response::Done
        ));
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn oversized_frames_are_rejected_before_allocating_the_payload() {
        let (mut server, mut client) = stream_pair().await;
        server.write_u32((MAX_MESSAGE + 1) as u32).await.unwrap();
        assert_eq!(
            read::<Frame>(&mut client).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[cfg(test)]
#[path = "rpc/concurrency_tests.rs"]
mod concurrency_tests;

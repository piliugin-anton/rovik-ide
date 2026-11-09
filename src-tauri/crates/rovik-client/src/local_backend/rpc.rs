//! Bounded typed RPC over the authenticated local stream. Calls are never
//! automatically retried: a lost reply may follow a committed mutation.

use super::{connect, Ready};
use crate::desktop_api::DesktopRequest;
use crate::desktop_backend::DesktopBackend;
#[cfg(test)]
use rovik_protocol::Frame;
use rovik_protocol::{Fail, Request, Response};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{io, path::Path, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::UnixStream,
    sync::Mutex,
};

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
    Ping,
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
    Pong,
}

async fn read<T: DeserializeOwned>(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<T> {
    let size = stream.read_u32().await? as usize;
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

/// One window's serialized connection. Transport failure or cancellation
/// invalidates it so a late reply cannot be mistaken for a later call.
pub struct Client {
    /// The backend instance and session this connection belongs to.
    pub ready: Ready,
    stream: Mutex<Option<UnixStream>>,
}

impl Client {
    /// Attaches to an existing backend without constructing a local Core.
    pub async fn connect(directory: &Path) -> io::Result<Self> {
        let (stream, ready) = connect(directory).await?;
        Ok(Self {
            ready,
            stream: Mutex::new(Some(stream)),
        })
    }

    /// Sends exactly one call. An IO error leaves completion uncertain;
    /// callers must refresh state before deciding whether to retry.
    pub async fn call(&self, request: Request) -> io::Result<Result<Response, Fail>> {
        self.call_on(None, request).await
    }

    /// Sends a host-specific operation (for example connector setup) to
    /// the named remote host, as the desktop's host commands do.
    pub async fn call_host(
        &self,
        host: &str,
        request: Request,
    ) -> io::Result<Result<Response, Fail>> {
        if host.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "A remote host ID is required",
            ));
        }
        self.call_on(Some(host.to_string()), request).await
    }

    async fn call_on(
        &self,
        host: Option<String>,
        request: Request,
    ) -> io::Result<Result<Response, Fail>> {
        match self
            .exchange(ClientFrame::Call {
                id: 1,
                req: Box::new(request),
                host,
            })
            .await?
        {
            ServerFrame::Reply { id: 1, res } => Ok(*res),
            _ => {
                self.stream.lock().await.take();
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Unexpected backend reply",
                ))
            }
        }
    }

    /// Runs a desktop-only operation without changing cloud protocol routing.
    pub async fn desktop(
        &self,
        request: DesktopRequest,
    ) -> io::Result<Result<serde_json::Value, serde_json::Value>> {
        match self
            .exchange(ClientFrame::Desktop {
                id: 1,
                req: Box::new(request),
            })
            .await?
        {
            ServerFrame::DesktopReply { id: 1, res } => Ok(res),
            _ => {
                self.stream.lock().await.take();
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Unexpected desktop reply",
                ))
            }
        }
    }

    async fn exchange(&self, frame: ClientFrame) -> io::Result<ServerFrame> {
        let mut slot = self.stream.lock().await;
        let mut stream = slot.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "Backend connection lost; refresh state before retrying",
            )
        })?;
        // Restore only after a complete matching response. Cancellation leaves
        // the connection unusable and never retries a possibly completed write.
        let uncertain = || {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "Backend reply timed out; operation completion is unknown",
            )
        };
        let result = async {
            tokio::time::timeout(IO_WAIT, write(&mut stream, &frame))
                .await
                .map_err(|_| uncertain())??;
            // Deployments and other desktop operations already own their
            // deadlines. Do not cut them off at the framing timeout. Dropping
            // the caller still invalidates this connection without a retry.
            let reply: ServerFrame = if matches!(frame, ClientFrame::Desktop { .. }) {
                read(&mut stream).await?
            } else {
                tokio::time::timeout(IO_WAIT, read(&mut stream))
                    .await
                    .map_err(|_| uncertain())??
            };
            let matches = matches!(
                (&frame, &reply),
                (
                    ClientFrame::Call { id: 1, .. },
                    ServerFrame::Reply { id: 1, .. }
                ) | (
                    ClientFrame::Desktop { id: 1, .. },
                    ServerFrame::DesktopReply { id: 1, .. }
                )
            );
            if !matches {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Unexpected backend reply",
                ));
            }
            Ok(reply)
        }
        .await;
        if result.is_ok() {
            *slot = Some(stream);
        }
        result
    }
}

/// Serves requests after the listener has authenticated the peer. A
/// disconnected client does not cancel an already accepted operation.
pub async fn serve(mut stream: UnixStream, backend: Arc<DesktopBackend>) -> io::Result<()> {
    loop {
        // An idle window may stay connected indefinitely. Once bytes arrive,
        // bound the time allowed to finish a frame to reject stalled senders.
        stream.readable().await?;
        let frame: ClientFrame = match tokio::time::timeout(IO_WAIT, read(&mut stream)).await {
            Ok(Ok(frame)) => frame,
            Ok(Err(e)) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Backend client did not finish its request",
                ))
            }
        };
        let reply = match frame {
            ClientFrame::Call { id, req, host } => ServerFrame::Reply {
                id,
                res: Box::new(match host {
                    Some(host) => backend.remote.host_call(&host, *req).await,
                    None => backend.dispatch(*req).await,
                }),
            },
            ClientFrame::Desktop { id, req } => ServerFrame::DesktopReply {
                id,
                res: req.dispatch(&backend).await,
            },
            ClientFrame::Ping => ServerFrame::Pong,
        };
        tokio::time::timeout(IO_WAIT, write(&mut stream, &reply))
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "Backend client stopped reading")
            })??;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn desktop_calls_reject_cloud_replies_and_do_not_reuse_the_connection() {
        let (client, mut server) = pair();
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

    fn pair() -> (Client, UnixStream) {
        let (client, server) = UnixStream::pair().unwrap();
        (
            Client {
                ready: Ready {
                    protocol: super::super::PROTOCOL,
                    boot: "test".into(),
                    version: "test".into(),
                    session: "test".into(),
                },
                stream: Mutex::new(Some(client)),
            },
            server,
        )
    }

    #[tokio::test]
    async fn a_lost_reply_invalidates_the_connection_without_retrying() {
        let (client, mut server) = pair();
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
        let (client, mut server) = pair();
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
    async fn cancelling_an_inflight_call_invalidates_the_connection() {
        let (client, mut server) = pair();
        let client = Arc::new(client);
        let calling = client.clone();
        let task = tokio::spawn(async move { calling.call(Request::AgentStatus).await });
        let _: Frame = read(&mut server).await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(
            client.call(Request::AgentStatus).await.unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
    }

    #[tokio::test]
    async fn cancelling_a_desktop_operation_invalidates_its_connection() {
        let (client, mut server) = pair();
        let client = Arc::new(client);
        let calling = client.clone();
        let task = tokio::spawn(async move {
            calling
                .desktop(DesktopRequest::from(
                    crate::desktop_api::hosts::HostsRequest::Upgrade { id: "host".into() },
                ))
                .await
        });
        let _: ClientFrame = read(&mut server).await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(
            client.call(Request::AgentStatus).await.unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
    }

    #[tokio::test]
    async fn explicit_host_calls_preserve_their_destination() {
        let (client, mut server) = pair();
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
        let (mut server, mut client) = UnixStream::pair().unwrap();
        server.write_u32((MAX_MESSAGE + 1) as u32).await.unwrap();
        assert_eq!(
            read::<Frame>(&mut client).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}

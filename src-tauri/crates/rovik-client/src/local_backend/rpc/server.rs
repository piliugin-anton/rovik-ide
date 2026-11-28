//! Concurrent dispatch preserves service ownership guards and session lifetimes.

use super::*;
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinSet;

struct Abort(tokio::task::AbortHandle);

impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Serves requests after the listener has authenticated the peer. Accepted
/// backend-owned work finishes even if its caller disconnects or stops waiting.
pub async fn serve(
    mut stream: Stream,
    backend: Arc<DesktopBackend>,
    session_id: String,
) -> io::Result<()> {
    let lease = backend
        .sessions
        .register(session_id)
        .map_err(io::Error::other)?;
    let frame = tokio::select! {
        biased;
        _ = lease.session.closed() => return Ok(()),
        frame = read_after_idle(&mut stream, IO_WAIT) => match frame {
            Ok(frame) => frame,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        },
    };
    let first = match frame {
        ClientFrame::Subscribe { session } => {
            let events = backend
                .sessions
                .get(&session)
                .and_then(|session| session.events());
            let result = events.as_ref().map(|_| ()).map_err(Clone::clone);
            write_reply(&mut stream, &ServerFrame::Subscribed { result }).await?;
            return match events {
                Ok(events) => serve_events(stream, events).await,
                Err(_) => Ok(()),
            };
        }
        ClientFrame::CloseSession { session } => {
            return close_sessions(stream, &backend, &lease.session, session).await;
        }
        ClientFrame::Ping => {
            write_reply(&mut stream, &ServerFrame::Pong).await?;
            None
        }
        frame => Some(frame),
    };
    commands(stream, backend, &lease.session, first).await
}

// Repeated detach acknowledgements are safe even after the owner disappeared.
// Keep this management connection independent from the owner's pending work.
async fn close_sessions(
    mut stream: Stream,
    backend: &DesktopBackend,
    owner: &crate::desktop_session::Session,
    mut target: String,
) -> io::Result<()> {
    loop {
        backend.sessions.close(&target, "Desktop client detached");
        write_reply(&mut stream, &ServerFrame::SessionClosed).await?;
        let frame = tokio::select! {
            biased;
            _ = owner.closed() => return Ok(()),
            frame = read_after_idle(&mut stream, IO_WAIT) => frame,
        };
        target = match frame {
            Ok(ClientFrame::CloseSession { session }) => session,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Unexpected desktop session control frame",
                ))
            }
        };
    }
}

async fn commands(
    stream: Stream,
    backend: Arc<DesktopBackend>,
    session: &Arc<crate::desktop_session::Session>,
    first: Option<ClientFrame>,
) -> io::Result<()> {
    let (mut input, mut output) = tokio::io::split(stream);
    let (received, mut incoming) = mpsc::channel(1);
    let reader = tokio::spawn(async move {
        if let Some(frame) = first {
            if received.send(Ok(frame)).await.is_err() {
                return;
            }
        }
        loop {
            let frame = read_after_idle(&mut input, IO_WAIT).await;
            let failed = frame.is_err();
            if received.send(frame).await.is_err() || failed {
                return;
            }
        }
    });
    let _reader = Abort(reader.abort_handle());
    let (replies, mut outgoing) = mpsc::channel(ORDINARY_CALLS + CONTROL_CALLS);
    let mut writer = tokio::spawn(async move {
        while let Some(frame) = outgoing.recv().await {
            write_reply(&mut output, &frame).await?;
        }
        Ok(())
    });
    let _writer = Abort(writer.abort_handle());
    let ordinary = Arc::new(Semaphore::new(ORDINARY_CALLS));
    let controls = Arc::new(Semaphore::new(CONTROL_CALLS));
    let mut jobs = JoinSet::new();
    let mut last_id = 0;
    let mut writer_finished = false;
    let result = loop {
        let received = tokio::select! {
            biased;
            _ = session.closed() => break Ok(()),
            result = &mut writer => {
                writer_finished = true;
                break result.map_err(io::Error::other).and_then(|result| result);
            },
            result = jobs.join_next(), if !jobs.is_empty() => {
                if let Some(Err(error)) = result {
                    break Err(io::Error::other(error));
                }
                continue;
            },
            frame = incoming.recv() => frame,
        };
        let frame = match received {
            Some(Ok(frame)) => frame,
            Some(Err(error)) if error.kind() != io::ErrorKind::UnexpectedEof => break Err(error),
            _ => break Ok(()),
        };
        if session.is_closed() {
            break Ok(());
        }
        let id = match &frame {
            ClientFrame::Call { id, .. } | ClientFrame::Desktop { id, .. } => *id,
            _ => {
                break Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Unexpected command connection frame",
                ))
            }
        };
        if id <= last_id {
            break Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Backend request IDs must increase",
            ));
        }
        last_id = id;
        let slots = if frame.is_control() {
            &controls
        } else {
            &ordinary
        };
        let permit = match slots.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                // Never await capacity on the reader: a later Stop must still
                // reach its reserved slots when ordinary work is saturated.
                let reply = failure(&frame, rovik_protocol::FailCode::Busy,
                    "Backend request capacity reached; wait for an operation to finish before retrying");
                if replies.send(reply).await.is_err() {
                    break Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "Backend reply writer stopped",
                    ));
                }
                continue;
            }
        };
        let backend = backend.clone();
        let session = session.clone();
        let replies = replies.clone();
        let mut job = Box::pin(async move {
            let _permit = permit;
            let fallback = failure(
                &frame,
                rovik_protocol::FailCode::Internal,
                "Backend operation failed unexpectedly; refresh its state before retrying",
            );
            let reply = AssertUnwindSafe(async move {
                match frame {
                    ClientFrame::Call { id, req, host } => ServerFrame::Reply {
                        id,
                        res: Box::new(match host {
                            Some(host) => backend.remote.host_call(&host, *req).await,
                            None => backend.dispatch(*req).await,
                        }),
                    },
                    ClientFrame::Desktop { id, req } => ServerFrame::DesktopReply {
                        id,
                        res: req.dispatch_in(&backend, Some(&session)).await,
                    },
                    _ => unreachable!("command frame was validated before admission"),
                }
            })
            .catch_unwind()
            .await
            .unwrap_or(fallback);
            // A lost reply never retries or cancels an accepted mutation.
            let _ = replies.send(reply).await;
        });
        // Give service admission its first poll in wire order, including the
        // per-chat start/Stop guards, then let pending work run independently.
        if futures_util::poll!(job.as_mut()).is_pending() {
            jobs.spawn(job);
        }
    };
    reader.abort();
    // Client-owned callbacks close promptly; backend-owned mutations continue.
    session.close("Desktop command connection ended");
    // Keep the session lease and host connection task alive until accepted work
    // finishes, so graceful retirement cannot abandon a committed operation.
    let mut result = result;
    while let Some(joined) = jobs.join_next().await {
        if let Err(error) = joined {
            if result.is_ok() {
                result = Err(io::Error::other(error));
            }
        }
    }
    drop(replies);
    if !writer_finished {
        let written = writer
            .await
            .map_err(io::Error::other)
            .and_then(|result| result);
        if result.is_ok() {
            result = written;
        }
    }
    result
}

fn failure(frame: &ClientFrame, code: rovik_protocol::FailCode, message: &str) -> ServerFrame {
    match frame {
        ClientFrame::Call { id, .. } => ServerFrame::Reply {
            id: *id,
            res: Box::new(Err(Fail::new(code, message))),
        },
        ClientFrame::Desktop { id, .. } => ServerFrame::DesktopReply {
            id: *id,
            res: Err(serde_json::Value::String(message.into())),
        },
        _ => unreachable!("only admitted commands produce operation failures"),
    }
}

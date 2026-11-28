//! Independent framing tasks multiplex one window's accepted operations.

use super::*;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use tokio::sync::{mpsc, oneshot, watch, OwnedSemaphorePermit, Semaphore};

struct Pending {
    desktop: bool,
    reply: oneshot::Sender<io::Result<ServerFrame>>,
    // Cancellation drops the waiter, not the accepted operation's slot.
    _permit: OwnedSemaphorePermit,
}

#[derive(Default)]
struct State {
    last_id: u64,
    pending: HashMap<u64, Pending>,
}

struct Transport {
    state: Mutex<State>,
    closing: watch::Sender<bool>,
    ordinary: Arc<Semaphore>,
    controls: Arc<Semaphore>,
}

impl Transport {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn fail(&self, error: io::Error) {
        let mut state = self.state();
        self.closing.send_replace(true);
        self.ordinary.close();
        self.controls.close();
        for (_, pending) in state.pending.drain() {
            let _ = pending
                .reply
                .send(Err(io::Error::new(error.kind(), error.to_string())));
        }
    }

    fn receive(&self, frame: ServerFrame) -> io::Result<()> {
        let (id, desktop) = match &frame {
            ServerFrame::Reply { id, .. } => (*id, false),
            ServerFrame::DesktopReply { id, .. } => (*id, true),
            _ => return Err(unexpected()),
        };
        let mut state = self.state();
        if !state
            .pending
            .get(&id)
            .is_some_and(|pending| pending.desktop == desktop)
        {
            return Err(unexpected());
        }
        if let Some(pending) = state.pending.remove(&id) {
            // A cancelled caller leaves its ID reserved until this reply.
            let _ = pending.reply.send(Ok(frame));
        }
        Ok(())
    }
}

/// One window's bounded, multiplexed connection. Cancelling a waiter does not
/// cancel or replay accepted work, or interfere with another operation's reply.
pub struct Client {
    /// The backend instance and session this connection belongs to.
    pub ready: Ready,
    pub(super) directory: std::path::PathBuf,
    transport: Arc<Transport>,
    outgoing: mpsc::Sender<ClientFrame>,
    tasks: [tokio::task::AbortHandle; 2],
}

impl Client {
    pub(crate) fn usable(&self) -> bool {
        !*self.transport.closing.borrow()
    }

    /// Discovery directory for an independent read-only event connection.
    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    /// Attaches to an existing backend without constructing a local Core.
    pub async fn connect(directory: &Path) -> io::Result<Self> {
        let (stream, ready) = connect(directory).await?;
        Self::from_connected(directory, stream, ready).await
    }

    pub(crate) async fn from_connected(
        directory: &Path,
        mut stream: Stream,
        ready: Ready,
    ) -> io::Result<Self> {
        // Establish the owner session before a separate event stream attaches.
        tokio::time::timeout(IO_WAIT, async {
            write(&mut stream, &ClientFrame::Ping).await?;
            match read(&mut stream).await? {
                ServerFrame::Pong => Ok(()),
                _ => Err(unexpected()),
            }
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Backend attachment timed out"))??;
        Ok(Self::start(directory, stream, ready))
    }

    pub(super) fn start(directory: &Path, stream: Stream, ready: Ready) -> Self {
        Self::start_with_frame_wait(directory, stream, ready, IO_WAIT)
    }

    pub(super) fn start_with_frame_wait(
        directory: &Path,
        stream: Stream,
        ready: Ready,
        frame_wait: Duration,
    ) -> Self {
        let transport = Arc::new(Transport {
            state: Mutex::new(State::default()),
            closing: watch::channel(false).0,
            ordinary: Arc::new(Semaphore::new(ORDINARY_CALLS)),
            controls: Arc::new(Semaphore::new(CONTROL_CALLS)),
        });
        let (outgoing, mut frames) = mpsc::channel(ORDINARY_CALLS + CONTROL_CALLS);
        let (mut input, mut output) = tokio::io::split(stream);
        let reader_state = transport.clone();
        let mut reader_closed = transport.closing.subscribe();
        let reader = tokio::spawn(async move {
            let result: io::Result<()> = async {
                loop {
                    if *reader_closed.borrow() {
                        return Ok(());
                    }
                    let frame = tokio::select! {
                        biased;
                        _ = reader_closed.changed() => return Ok(()),
                        frame = read_after_idle(&mut input, frame_wait) => frame?,
                    };
                    reader_state.receive(frame)?;
                }
            }
            .await;
            if let Err(error) = result {
                reader_state.fail(error);
            }
        });
        let writer_state = transport.clone();
        let mut writer_closed = transport.closing.subscribe();
        let writer = tokio::spawn(async move {
            let result: io::Result<()> = async {
                loop {
                    if *writer_closed.borrow() {
                        return Ok(());
                    }
                    let frame = tokio::select! {
                        biased;
                        _ = writer_closed.changed() => return Ok(()),
                        frame = frames.recv() => match frame {
                            Some(frame) => frame,
                            None => return Ok(()),
                        },
                    };
                    // The caller cannot drop a partially written frame.
                    tokio::select! {
                        biased;
                        _ = writer_closed.changed() => return Ok(()),
                        result = write_reply(&mut output, &frame) => result?,
                    }
                }
            }
            .await;
            if let Err(error) = result {
                writer_state.fail(error);
            }
        });
        Self {
            ready,
            directory: directory.to_path_buf(),
            transport,
            outgoing,
            tasks: [reader.abort_handle(), writer.abort_handle()],
        }
    }

    /// Releases this client's resources without stopping backend-owned work.
    /// An independent control connection can detach even during a stalled call.
    pub async fn close(&self) -> io::Result<()> {
        self.transport.fail(detached());
        for task in &self.tasks {
            task.abort();
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            let (mut control, _) = connect(&self.directory).await?;
            write(
                &mut control,
                &ClientFrame::CloseSession {
                    session: self.ready.session.clone(),
                },
            )
            .await?;
            match read(&mut control).await? {
                ServerFrame::SessionClosed => Ok(()),
                _ => Err(unexpected()),
            }
        })
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "Desktop client detachment timed out",
            )
        })?
    }

    /// Attaches an independent event stream, so output never waits behind a call.
    pub async fn events(&self) -> io::Result<EventStream> {
        if !self.usable() {
            return Err(detached());
        }
        let (mut stream, _) = connect(&self.directory).await?;
        tokio::time::timeout(IO_WAIT, async {
            write(
                &mut stream,
                &ClientFrame::Subscribe {
                    session: self.ready.session.clone(),
                },
            )
            .await?;
            match read(&mut stream).await? {
                ServerFrame::Subscribed { result: Ok(()) } => Ok(EventStream { stream }),
                ServerFrame::Subscribed { result: Err(error) } => Err(io::Error::other(error)),
                _ => Err(unexpected()),
            }
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Event subscription timed out"))?
    }

    /// Sends exactly one call. An IO error leaves completion uncertain;
    /// callers must refresh state before deciding whether to retry.
    pub async fn call(&self, request: Request) -> io::Result<Result<Response, Fail>> {
        self.call_on(None, request).await
    }

    /// Sends a host-specific operation to the named remote host.
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
                id: 0,
                req: Box::new(request),
                host,
            })
            .await?
        {
            ServerFrame::Reply { res, .. } => Ok(*res),
            _ => Err(unexpected()),
        }
    }

    /// Runs a desktop-only operation without changing cloud protocol routing.
    pub async fn desktop(
        &self,
        request: DesktopRequest,
    ) -> io::Result<Result<serde_json::Value, serde_json::Value>> {
        match self
            .exchange(ClientFrame::Desktop {
                id: 0,
                req: Box::new(request),
            })
            .await?
        {
            ServerFrame::DesktopReply { res, .. } => Ok(res),
            _ => Err(unexpected()),
        }
    }

    async fn exchange(&self, mut frame: ClientFrame) -> io::Result<ServerFrame> {
        let slots = if frame.is_control() {
            &self.transport.controls
        } else {
            &self.transport.ordinary
        };
        let permit = slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| disconnected())?;
        // Reserve the queue first: a cancelled queue waiter must not leave an
        // unsent ID in the pending map. Neither step below yields after insertion.
        let queued = self.outgoing.reserve().await.map_err(|_| disconnected())?;
        let (reply, received) = oneshot::channel();
        {
            let mut state = self.transport.state();
            if !self.usable() {
                return Err(disconnected());
            }
            let id = state.last_id.checked_add(1).ok_or_else(|| {
                io::Error::other("Backend request IDs exhausted; reconnect before continuing")
            })?;
            let desktop = match &mut frame {
                ClientFrame::Call { id: slot, .. } => {
                    *slot = id;
                    false
                }
                ClientFrame::Desktop { id: slot, .. } => {
                    *slot = id;
                    true
                }
                _ => return Err(unexpected()),
            };
            state.last_id = id;
            state.pending.insert(
                id,
                Pending {
                    desktop,
                    reply,
                    _permit: permit,
                },
            );
            queued.send(frame);
        }
        // Execution owns its deadline. Framing has a timeout, but preparation,
        // deployments and remote operations may legitimately take much longer.
        received.await.map_err(|_| disconnected())?
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.transport.fail(detached());
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn unexpected() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Unexpected backend reply")
}

fn disconnected() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotConnected,
        "Backend connection lost; refresh state before retrying",
    )
}

fn detached() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotConnected,
        "Desktop client detached; an accepted operation may still complete",
    )
}

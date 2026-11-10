//! The desktop command boundary. Embedded mode supports incremental
//! migration; IPC mode never constructs or falls back to an embedded Core.

use crate::desktop_backend::DesktopBackend;
use rovik_protocol::{Fail, Request, Response};
use std::sync::Arc;

/// One explicit backend destination for a window's adapted commands.
pub struct Connection {
    #[cfg(unix)]
    journal_reader: tokio::sync::Mutex<Option<crate::local_backend::rpc::Client>>,
    closing: tokio::sync::watch::Sender<bool>,
    backend: Backend,
    events: crate::desktop_events::EventRouter,
}

enum Backend {
    Embedded {
        backend: Arc<DesktopBackend>,
        lease: crate::desktop_session::SessionLease,
    },
    #[cfg(unix)]
    Local(crate::local_backend::rpc::Client),
}

impl Connection {
    pub(super) fn usable(&self) -> bool {
        if *self.closing.borrow() || self.events.failure().is_some() {
            return false;
        }
        match &self.backend {
            Backend::Embedded { lease, .. } => !lease.session.is_closed(),
            #[cfg(unix)]
            Backend::Local(client) => client.usable(),
        }
    }

    /// Detaches only this window. Execution and mounts remain owned by the host.
    /// Repeating this call retries the idempotent server acknowledgement.
    pub async fn close(&self) -> Result<(), String> {
        self.closing.send_replace(true);
        self.events.close();
        let result = match &self.backend {
            Backend::Embedded { lease, .. } => {
                lease.session.close("Desktop client detached");
                Ok(())
            }
            #[cfg(unix)]
            Backend::Local(client) => client.close().await.map_err(|error| error.to_string()),
        };
        #[cfg(unix)]
        if let Ok(mut reader) = self.journal_reader.try_lock() {
            reader.take();
        }
        result
    }

    /// Reads shared work independently of command and private-resource connections.
    /// Only this read connection is reopened after failure; commands are never replayed.
    pub async fn poll_events(
        &self,
        scope: crate::desktop_journal::Scope,
        cursor: Option<crate::desktop_journal::Cursor>,
    ) -> Result<serde_json::Value, String> {
        self.read_events(crate::desktop_api::DesktopRequest::Events { scope, cursor })
            .await
    }

    /// Reads scoped run state using the reconnectable read connection.
    pub async fn event_status(
        &self,
        scope: crate::desktop_journal::Scope,
    ) -> Result<serde_json::Value, String> {
        self.read_events(crate::desktop_api::DesktopRequest::EventStatus { scope })
            .await
    }

    async fn read_events(
        &self,
        request: crate::desktop_api::DesktopRequest,
    ) -> Result<serde_json::Value, String> {
        let mut closing = self.closing.subscribe();
        if *closing.borrow() {
            return Err("Desktop client detached".into());
        }
        tokio::select! {
            biased;
            _ = closing.changed() => Err("Desktop client detached".into()),
            reply = self.read_events_open(request) => reply,
        }
    }

    async fn read_events_open(
        &self,
        request: crate::desktop_api::DesktopRequest,
    ) -> Result<serde_json::Value, String> {
        match &self.backend {
            Backend::Embedded { backend, .. } => {
                request.dispatch_in(backend, None).await.map_err(|e| {
                    e.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| e.to_string())
                })
            }
            #[cfg(unix)]
            Backend::Local(client) => {
                let mut reader = self.journal_reader.lock().await;
                if reader.is_none() {
                    *reader = Some(
                        crate::local_backend::rpc::Client::connect(client.directory())
                            .await
                            .map_err(|e| e.to_string())?,
                    );
                }
                let current = reader.as_ref().ok_or("Event reader unavailable")?;
                match current.desktop(request).await {
                    Ok(reply) => reply.map_err(|e| {
                        e.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| e.to_string())
                    }),
                    Err(error) => {
                        *reader = None;
                        Err(format!("Desktop events disconnected: {error}"))
                    }
                }
            }
        }
    }

    /// Reports event loss to the native UI. A failed session must be reconnected.
    pub fn on_event_error(&self, report: impl Fn(String) + Send + Sync + 'static) {
        self.events.on_error(report);
    }

    /// Starts a resource with its callback already registered. Closing the
    /// native callback stops only that resource, using the private event link.
    pub async fn desktop_stream(
        &self,
        request: impl FnOnce(String) -> crate::desktop_api::DesktopRequest,
        callback: impl Fn(serde_json::Value) -> bool + Send + Sync + 'static,
    ) -> crate::desktop_api::DesktopResult {
        use crate::desktop_events::EventSource;
        self.events
            .start(|| async {
                match &self.backend {
                    Backend::Embedded { lease, .. } => {
                        lease.session.events().map(EventSource::Embedded)
                    }
                    #[cfg(unix)]
                    Backend::Local(client) => client
                        .events()
                        .await
                        .map(EventSource::Local)
                        .map_err(|e| e.to_string()),
                }
            })
            .await
            .map_err(serde_json::Value::String)?;
        let sink = self
            .events
            .register(callback)
            .map_err(serde_json::Value::String)?;
        // Cancellation can happen before the start reply. Do not leave its
        // native callback (or an embedded producer) alive with no caller.
        struct Pending<'a> {
            connection: &'a Connection,
            sink: String,
            active: bool,
        }
        impl Drop for Pending<'_> {
            fn drop(&mut self) {
                if self.active {
                    self.connection.events.remove(&self.sink);
                    match &self.connection.backend {
                        Backend::Embedded { lease, .. } => lease.session.cancel_sink(&self.sink),
                        #[cfg(unix)]
                        Backend::Local(_) => {}
                    }
                }
            }
        }
        let mut pending = Pending {
            connection: self,
            sink: sink.clone(),
            active: true,
        };
        let result = self.desktop_raw(request(sink)).await;
        pending.active = result.is_err();
        result
    }

    /// Waits for a finite operation's result after delivering all its progress.
    /// The startup RPC completes immediately, leaving controls such as Stop usable.
    pub async fn desktop_task(
        &self,
        request: impl FnOnce(String) -> crate::desktop_api::DesktopRequest,
        callback: impl Fn(serde_json::Value) -> Result<(), String> + Send + Sync + 'static,
    ) -> crate::desktop_api::DesktopResult {
        use crate::desktop_task::TaskEvent;
        use serde_json::Value;
        let (done, result) = tokio::sync::oneshot::channel();
        let done = std::sync::Mutex::new(Some(done));
        self.desktop_stream(request, move |value| {
            let (complete, accepted) = match serde_json::from_value::<TaskEvent>(value) {
                Ok(TaskEvent::Event(event)) => match callback(event) {
                    Ok(()) => return true,
                    Err(error) => (Err(Value::String(error)), false),
                },
                Ok(TaskEvent::Complete(result)) => (result, true),
                Err(error) => (
                    Err(Value::String(format!(
                        "Invalid desktop task event: {error}"
                    ))),
                    false,
                ),
            };
            if let Some(done) = done
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
            {
                let _ = done.send(complete);
            }
            accepted
        })
        .await?;
        result.await.map_err(|_| {
            Value::String(
                self.events.failure().unwrap_or_else(|| {
                    "Desktop task callback ended before its result arrived".into()
                }),
            )
        })?
    }

    /// Runs a desktop operation and preserves its existing UI reply shape.
    pub async fn desktop(
        &self,
        request: crate::desktop_api::DesktopRequest,
    ) -> Result<serde_json::Value, String> {
        self.desktop_raw(request)
            .await
            .map_err(|error| match error {
                serde_json::Value::String(message) => message,
                error => error.to_string(),
            })
    }

    /// Decodes a compact binary desktop reply for a native byte response.
    pub async fn desktop_bytes(
        &self,
        request: crate::desktop_api::DesktopRequest,
    ) -> Result<Vec<u8>, String> {
        use base64::Engine;
        let encoded: String = serde_json::from_value(self.desktop(request).await?)
            .map_err(|e| format!("Invalid binary desktop reply: {e}"))?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| format!("Invalid binary desktop reply: {e}"))
    }

    /// Preserves structured errors for commands such as host deployment.
    pub async fn desktop_raw(
        &self,
        request: crate::desktop_api::DesktopRequest,
    ) -> crate::desktop_api::DesktopResult {
        if let Some(error) = self.events.failure() {
            return Err(serde_json::Value::String(error));
        }
        match &self.backend {
            Backend::Embedded { backend, lease } => {
                if lease.session.is_closed() {
                    return Err(serde_json::Value::String(
                        "Desktop client session ended".into(),
                    ));
                }
                request.dispatch_in(backend, Some(&lease.session)).await
            }
            #[cfg(unix)]
            Backend::Local(client) => client
                .desktop(request)
                .await
                .map_err(|e| serde_json::Value::String(transport(e).message))?,
        }
    }

    /// Uses a private host on platforms awaiting IPC, or in embedded tests.
    pub fn embedded(backend: Arc<DesktopBackend>) -> Self {
        let lease = backend.sessions.create();
        Self {
            backend: Backend::Embedded { backend, lease },
            events: Default::default(),
            closing: tokio::sync::watch::channel(false).0,
            #[cfg(unix)]
            journal_reader: Default::default(),
        }
    }

    /// Attaches to the single local host, launching the packaged command only
    /// when discovery is absent. Authentication or compatibility errors fail closed.
    #[cfg(unix)]
    pub async fn attach(
        command: std::process::Command,
        data: &std::path::Path,
        config: &std::path::Path,
    ) -> std::io::Result<Self> {
        let (stream, ready) = crate::local_backend::ensure_command(command, data, config).await?;
        let directory = crate::local_backend::directory(data);
        let client =
            crate::local_backend::rpc::Client::from_connected(&directory, stream, ready).await?;
        Ok(Self::local(client))
    }

    /// Attaches to an existing local backend. Failure is returned to the
    /// window; a second private backend is never initialized here.
    #[cfg(unix)]
    pub async fn connect(directory: &std::path::Path) -> std::io::Result<Self> {
        let client = crate::local_backend::rpc::Client::connect(directory).await?;
        Ok(Self::local(client))
    }

    #[cfg(unix)]
    fn local(client: crate::local_backend::rpc::Client) -> Self {
        Self {
            backend: Backend::Local(client),
            events: Default::default(),
            closing: tokio::sync::watch::channel(false).0,
            journal_reader: Default::default(),
        }
    }

    /// Runs a typed operation using the backend's authoritative routing.
    pub async fn request(&self, request: Request) -> Result<Response, Fail> {
        if let Some(error) = self.events.failure() {
            return Err(Fail::new(rovik_protocol::FailCode::Unreachable, error));
        }
        match &self.backend {
            Backend::Embedded { backend, lease } => {
                if lease.session.is_closed() {
                    return Err(Fail::new(
                        rovik_protocol::FailCode::Unreachable,
                        "Desktop client session ended",
                    ));
                }
                backend.dispatch(request).await
            }
            #[cfg(unix)]
            Backend::Local(client) => client.call(request).await.map_err(transport)?,
        }
    }

    /// Runs an operation on an explicitly selected remote host.
    pub async fn host_request(&self, host: &str, request: Request) -> Result<Response, Fail> {
        if let Some(error) = self.events.failure() {
            return Err(Fail::new(rovik_protocol::FailCode::Unreachable, error));
        }
        if host.is_empty() {
            return Err(Fail::new(
                rovik_protocol::FailCode::BadRequest,
                "A remote host ID is required",
            ));
        }
        match &self.backend {
            Backend::Embedded { backend, lease } => {
                if lease.session.is_closed() {
                    return Err(Fail::new(
                        rovik_protocol::FailCode::Unreachable,
                        "Desktop client session ended",
                    ));
                }
                backend.remote.host_call(host, request).await
            }
            #[cfg(unix)]
            Backend::Local(client) => client.call_host(host, request).await.map_err(transport)?,
        }
    }
}

#[cfg(unix)]
fn transport(error: std::io::Error) -> Fail {
    let code = match error.kind() {
        std::io::ErrorKind::Unsupported => rovik_protocol::FailCode::Version,
        _ => rovik_protocol::FailCode::Unreachable,
    };
    Fail::new(code, format!("Local backend connection failed: {error}"))
}

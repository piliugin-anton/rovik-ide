//! The desktop command boundary. Embedded mode supports incremental
//! migration; IPC mode never constructs or falls back to an embedded Core.

use crate::desktop_backend::DesktopBackend;
use rovik_protocol::{Fail, Request, Response};
use std::sync::Arc;

/// One explicit backend destination for a window's adapted commands.
pub struct DesktopConnection {
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

impl DesktopConnection {
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
            connection: &'a DesktopConnection,
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

    /// Uses the application's existing backend during command migration.
    pub fn embedded(backend: Arc<DesktopBackend>) -> Self {
        let lease = backend.sessions.create();
        Self {
            backend: Backend::Embedded { backend, lease },
            events: Default::default(),
        }
    }

    /// Attaches to an existing local backend. Failure is returned to the
    /// window; a second private backend is never initialized here.
    #[cfg(unix)]
    pub async fn connect(directory: &std::path::Path) -> std::io::Result<Self> {
        let client = crate::local_backend::rpc::Client::connect(directory).await?;
        Ok(Self {
            backend: Backend::Local(client),
            events: Default::default(),
        })
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

    /// Converts a reply to the command's established return type.
    pub async fn call<T>(&self, request: Request) -> Result<T, String>
    where
        T: TryFrom<Response, Error = Fail>,
    {
        self.request(request)
            .await
            .and_then(T::try_from)
            .map_err(|e| e.message)
    }

    /// The typed reply of an operation on a selected host, or this backend
    /// when no host was selected.
    pub async fn call_on<T>(&self, host: Option<&str>, request: Request) -> Result<T, String>
    where
        T: TryFrom<Response, Error = Fail>,
    {
        let reply = match host.filter(|id| !id.is_empty()) {
            Some(host) => self.host_request(host, request).await,
            None => self.request(request).await,
        };
        reply.and_then(T::try_from).map_err(|e| e.message)
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

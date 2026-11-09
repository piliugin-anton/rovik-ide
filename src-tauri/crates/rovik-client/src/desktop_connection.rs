//! The desktop command boundary. Embedded mode supports incremental
//! migration; IPC mode never constructs or falls back to an embedded Core.

use crate::desktop_backend::DesktopBackend;
use rovik_protocol::{Fail, Request, Response};
use std::sync::Arc;

/// One explicit backend destination for a window's adapted commands.
pub struct DesktopConnection(Backend);

enum Backend {
    Embedded(Arc<DesktopBackend>),
    #[cfg(unix)]
    Local(crate::local_backend::rpc::Client),
}

impl DesktopConnection {
    /// Runs a desktop operation and preserves its existing UI reply shape.
    pub async fn desktop(
        &self,
        request: crate::desktop_api::DesktopRequest,
    ) -> Result<serde_json::Value, String> {
        match &self.0 {
            Backend::Embedded(backend) => request.dispatch(backend).await,
            #[cfg(unix)]
            Backend::Local(client) => client
                .desktop(request)
                .await
                .map_err(|e| transport(e).message)?,
        }
    }

    /// Uses the application's existing backend during command migration.
    pub fn embedded(backend: Arc<DesktopBackend>) -> Self {
        Self(Backend::Embedded(backend))
    }

    /// Attaches to an existing local backend. Failure is returned to the
    /// window; a second private backend is never initialized here.
    #[cfg(unix)]
    pub async fn connect(directory: &std::path::Path) -> std::io::Result<Self> {
        let client = crate::local_backend::rpc::Client::connect(directory).await?;
        Ok(Self(Backend::Local(client)))
    }

    /// Runs a typed operation using the backend's authoritative routing.
    pub async fn request(&self, request: Request) -> Result<Response, Fail> {
        match &self.0 {
            Backend::Embedded(backend) => backend.dispatch(request).await,
            #[cfg(unix)]
            Backend::Local(client) => client.call(request).await.map_err(transport)?,
        }
    }

    /// Runs an operation on an explicitly selected remote host.
    pub async fn host_request(&self, host: &str, request: Request) -> Result<Response, Fail> {
        if host.is_empty() {
            return Err(Fail::new(
                rovik_protocol::FailCode::BadRequest,
                "A remote host ID is required",
            ));
        }
        match &self.0 {
            Backend::Embedded(backend) => backend.remote.host_call(host, request).await,
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

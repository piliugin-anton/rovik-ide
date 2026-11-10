//! Desktop-only operations. Inputs are explicitly typed; replies preserve
//! the service's serialized UI contract without requiring display-only
//! types (including static labels) to be deserialized into the window.

use crate::desktop_backend::DesktopBackend;
use serde::{Deserialize, Serialize};

pub mod agent;
pub mod chat;
pub mod clm;
pub mod filesystem;
pub mod git;
pub mod hosts;
pub mod lsp;
pub mod mcp;
pub mod memory;
pub mod network;
pub mod overlay;
pub mod projects;
pub mod settings;
pub mod terminal;
pub mod vfs;

/// A desktop response preserves both string and structured UI errors.
pub type DesktopResult = Result<serde_json::Value, serde_json::Value>;

/// Operations owned by the local desktop backend, separate from cloud RPC.
#[derive(Serialize, Deserialize)]
#[serde(tag = "service", content = "request", rename_all = "camelCase")]
pub enum DesktopRequest {
    /// Client-owned language servers and their installation.
    Lsp(Box<lsp::LspRequest>),
    /// Backend-owned Git operations.
    Git(Box<git::GitRequest>),
    /// Scoped run-state recovery for a UI event reader.
    EventStatus {
        scope: crate::desktop_journal::Scope,
    },
    /// Read-only shared event replay, safe on a dedicated reconnectable connection.
    Events {
        scope: crate::desktop_journal::Scope,
        cursor: Option<crate::desktop_journal::Cursor>,
    },
    /// Client-owned project lifecycle.
    Projects(Box<projects::ProjectRequest>),
    /// Client-scoped model requests and estimates.
    Agent(Box<agent::AgentRequest>),
    /// Client-scoped file operations and watches.
    Filesystem(Box<filesystem::FilesystemRequest>),
    /// Backend-owned clm operations.
    Clm(Box<clm::ClmRequest>),
    /// Backend-owned chat operations.
    Chat(Box<chat::ChatRequest>),
    /// Shared AI and project settings.
    Settings(Box<settings::SettingsRequest>),
    /// Backend-owned mcp operations.
    Mcp(Box<mcp::McpRequest>),
    /// Backend-owned hosts operations.
    Hosts(Box<hosts::HostsRequest>),
    /// Project memories and their remote synchronization.
    Memory(Box<memory::MemoryRequest>),
    /// Network settings and credentials.
    Network(Box<network::NetworkRequest>),
    /// Chat overlays, including imported remote results.
    Overlay(Box<overlay::OverlayRequest>),
    /// The backend's virtual filesystem and mount.
    Vfs(Box<vfs::VfsRequest>),
    /// Client-owned local and remote terminals.
    Terminal(Box<terminal::TerminalRequest>),
}

impl DesktopRequest {
    #[cfg(test)]
    pub(crate) async fn dispatch(self, backend: &DesktopBackend) -> DesktopResult {
        self.dispatch_in(backend, None).await
    }

    pub(crate) fn dispatch_in<'a>(
        self,
        backend: &'a DesktopBackend,
        session: Option<&'a std::sync::Arc<crate::desktop_session::Session>>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DesktopResult> + Send + 'a>> {
        // Each service has its own async state. Keep their combined dispatcher
        // on the heap so adding commands cannot exhaust a UI or RPC task's stack.
        Box::pin(async move {
            match self {
                Self::Lsp(request) => request.dispatch(backend, session).await,
                Self::Git(request) => request.dispatch(session).await,
                Self::EventStatus { scope } => {
                    encode_result(crate::desktop_journal::status(backend, scope).await)
                }
                Self::Events { scope, cursor } => {
                    encode_result(backend.journal.poll(backend, scope, cursor).await)
                }
                Self::Projects(request) => request.dispatch(backend, session).await,
                Self::Agent(request) => request.dispatch(backend, session).await,
                Self::Filesystem(request) => request
                    .dispatch(backend, session)
                    .await
                    .map_err(serde_json::Value::String),
                Self::Terminal(request) => request
                    .dispatch(backend, session)
                    .await
                    .map_err(serde_json::Value::String),
                Self::Clm(request) => request.dispatch(backend).await,
                Self::Chat(request) => request.dispatch(backend).await,
                Self::Settings(request) => request.dispatch(backend).await,
                Self::Mcp(request) => request.dispatch(backend, session).await,
                Self::Hosts(request) => request.dispatch(backend, session).await,
                Self::Overlay(request) => request
                    .dispatch(backend)
                    .await
                    .map_err(serde_json::Value::String),
                Self::Vfs(request) => request
                    .dispatch(backend, session)
                    .await
                    .map_err(serde_json::Value::String),
                Self::Memory(request) => request.dispatch(backend).await,
                Self::Network(request) => request.dispatch(backend).await,
            }
        })
    }
}

fn encode(value: impl Serialize) -> Result<serde_json::Value, String> {
    serde_json::to_value(value).map_err(|e| format!("Could not encode desktop reply: {e}"))
}

fn encode_result<T: Serialize, E: Serialize>(result: Result<T, E>) -> DesktopResult {
    match result {
        Ok(value) => encode(value).map_err(serde_json::Value::String),
        Err(error) => Err(encode(error).unwrap_or_else(serde_json::Value::String)),
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    rovik_core::rt::spawn_blocking(work)
        .await
        .map_err(|e| e.to_string())?
}

// Compact binary fields avoid JSON's per-byte integer expansion. The frame
// bound still caps allocations before decoding; attachment validation applies
// its own image/PDF limits in the backend.
mod bytes {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .map_err(serde::de::Error::custom)
    }
}

impl From<clm::ClmRequest> for DesktopRequest {
    fn from(request: clm::ClmRequest) -> Self {
        Self::Clm(Box::new(request))
    }
}

impl From<chat::ChatRequest> for DesktopRequest {
    fn from(request: chat::ChatRequest) -> Self {
        Self::Chat(Box::new(request))
    }
}

impl From<settings::SettingsRequest> for DesktopRequest {
    fn from(request: settings::SettingsRequest) -> Self {
        Self::Settings(Box::new(request))
    }
}

impl From<mcp::McpRequest> for DesktopRequest {
    fn from(request: mcp::McpRequest) -> Self {
        Self::Mcp(Box::new(request))
    }
}

impl From<hosts::HostsRequest> for DesktopRequest {
    fn from(request: hosts::HostsRequest) -> Self {
        Self::Hosts(Box::new(request))
    }
}

impl From<memory::MemoryRequest> for DesktopRequest {
    fn from(request: memory::MemoryRequest) -> Self {
        Self::Memory(Box::new(request))
    }
}

impl From<network::NetworkRequest> for DesktopRequest {
    fn from(request: network::NetworkRequest) -> Self {
        Self::Network(Box::new(request))
    }
}

impl From<overlay::OverlayRequest> for DesktopRequest {
    fn from(request: overlay::OverlayRequest) -> Self {
        Self::Overlay(Box::new(request))
    }
}

impl From<vfs::VfsRequest> for DesktopRequest {
    fn from(request: vfs::VfsRequest) -> Self {
        Self::Vfs(Box::new(request))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_bytes_round_trip_without_integer_array_expansion() {
        let bytes = vec![0, 127, 128, 255];
        let request = DesktopRequest::from(chat::ChatRequest::StoreAttachment {
            bytes: bytes.clone(),
        });
        let wire = serde_json::to_value(request).unwrap();
        assert!(wire["request"]["bytes"].is_string());
        let DesktopRequest::Chat(request) = serde_json::from_value(wire).unwrap() else {
            panic!("wrong request")
        };
        let chat::ChatRequest::StoreAttachment { bytes: decoded } = *request else {
            panic!("wrong chat operation")
        };
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn structured_errors_keep_their_discriminant_and_fields() {
        let error = crate::hosts_page::DeployFailure::NeedsRoot {
            command: "install".into(),
            message: "administrator required".into(),
        };
        let result = encode_result::<(), _>(Err(error)).unwrap_err();
        assert_eq!(result["kind"], "needsRoot");
        assert_eq!(result["command"], "install");
    }
}

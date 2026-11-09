//! Desktop-only operations. Inputs are explicitly typed; replies preserve
//! the service's serialized UI contract without requiring display-only
//! types (including static labels) to be deserialized into the window.

use crate::desktop_backend::DesktopBackend;
use serde::{Deserialize, Serialize};

pub mod overlay;
pub mod vfs;

/// Operations owned by the local desktop backend, separate from cloud RPC.
#[derive(Serialize, Deserialize)]
#[serde(tag = "service", content = "request", rename_all = "camelCase")]
pub enum DesktopRequest {
    /// Chat overlays, including imported remote results.
    Overlay(overlay::OverlayRequest),
    /// The backend's virtual filesystem and mount.
    Vfs(vfs::VfsRequest),
}

impl DesktopRequest {
    pub(crate) async fn dispatch(
        self,
        backend: &DesktopBackend,
    ) -> Result<serde_json::Value, String> {
        match self {
            Self::Overlay(request) => request.dispatch(backend).await,
            Self::Vfs(request) => request.dispatch(backend).await,
        }
    }
}

fn encode(value: impl Serialize) -> Result<serde_json::Value, String> {
    serde_json::to_value(value).map_err(|e| format!("Could not encode desktop reply: {e}"))
}

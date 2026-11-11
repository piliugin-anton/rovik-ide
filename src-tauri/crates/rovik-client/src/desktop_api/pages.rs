//! Private page-opening callbacks; native actions stay in their owning window.
use super::{encode_result, DesktopRequest, DesktopResult};
use crate::{desktop_backend::DesktopBackend, desktop_journal::Scope, desktop_session::Session};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Page callbacks are leased resources, not replayable shared events.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum PagesRequest {
    /// Replaces this client's page callback with its current project/chat scope.
    Subscribe { scope: Scope, sink: String },
    /// Ends only a callback owned by this client; stale IDs cannot end a replacement.
    Unsubscribe { id: String },
}
impl PagesRequest {
    pub(crate) fn dispatch(
        self,
        backend: &DesktopBackend,
        session: Option<&Arc<Session>>,
    ) -> DesktopResult {
        let owner = session.ok_or_else(|| {
            serde_json::Value::String("A client session is required for page callbacks".into())
        })?;
        match self {
            Self::Subscribe { scope, sink } => {
                encode_result(backend.pages.subscribe(owner, sink, scope))
            }
            Self::Unsubscribe { id } => {
                encode_result(Ok::<_, String>(owner.remove(&format!("pages:{id}"))))
            }
        }
    }
}
impl From<PagesRequest> for DesktopRequest {
    fn from(request: PagesRequest) -> Self {
        Self::Pages(Box::new(request))
    }
}

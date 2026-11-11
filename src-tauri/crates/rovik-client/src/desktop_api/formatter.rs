//! Editor formatter discovery and execution are finite, client-owned tasks.
use super::{encode_result, DesktopRequest, DesktopResult};
use crate::{desktop_backend::DesktopBackend, desktop_session::Session};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// The existing editor buffer contract; no source file is written by the backend.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum FormatterRequest {
    /// Discovers the project's formatter off the RPC connection thread.
    Lookup {
        path: String,
        language: String,
        root: Option<String>,
        sink: String,
    },
    /// Runs the selected formatter on the unsaved buffer.
    Format {
        path: String,
        language: String,
        root: Option<String>,
        content: String,
        sink: String,
    },
}
impl FormatterRequest {
    pub(crate) fn dispatch(
        self,
        backend: &DesktopBackend,
        owner: Option<&Arc<Session>>,
    ) -> DesktopResult {
        let owner = owner.ok_or_else(|| {
            serde_json::Value::String("A client session is required for formatting".into())
        })?;
        let state = backend.formatters.clone();
        encode_result(match self {
            Self::Lookup {
                path,
                language,
                root,
                sink,
            } => crate::desktop_task::start(owner, sink, move |_| async move {
                encode_result(
                    crate::formatter::formatter_for_file(state, path, language, root).await,
                )
            }),
            Self::Format {
                path,
                language,
                root,
                content,
                sink,
            } => crate::desktop_task::start(owner, sink, move |_| async move {
                encode_result(
                    crate::formatter::format_file(state, path, language, root, content).await,
                )
            }),
        })
    }
}
impl From<FormatterRequest> for DesktopRequest {
    fn from(request: FormatterRequest) -> Self {
        Self::Formatter(Box::new(request))
    }
}

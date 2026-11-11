//! Browser operations execute in the shared backend; native code only forwards.
use super::{encode_result, DesktopRequest, DesktopResult};
use crate::{desktop_backend::DesktopBackend, desktop_session::Session};
use rovik_core::browser_tools;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Existing browser command inputs and results, carried over desktop RPC.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum BrowserRequest {
    /// Reads the pinned Chrome installation status.
    Status,
    /// Observes the backend's shared installation through a private callback.
    Install { sink: String },
    /// Searches using the existing backend browser service.
    Search {
        query: String,
        max_results: Option<usize>,
        sink: String,
    },
    /// Reads an HTTP(S) page using the backend's URL and network checks.
    Browse {
        url: String,
        max_chars: Option<usize>,
        sink: String,
    },
    /// Captures the browser's current page or the supplied URL into an attachment.
    Screenshot { url: Option<String>, sink: String },
}
impl BrowserRequest {
    pub(crate) async fn dispatch(
        self,
        backend: &DesktopBackend,
        owner: Option<&Arc<Session>>,
    ) -> DesktopResult {
        if matches!(self, Self::Status) {
            return encode_result(Ok::<_, String>(crate::browser_install::status()));
        }
        let owner = owner.ok_or_else(|| {
            serde_json::Value::String("A client session is required for browser operations".into())
        })?;
        let core = backend.core.clone();
        let result = match self {
            Self::Status => unreachable!(),
            Self::Install { sink } => {
                let installer = backend.browser_install.clone();
                crate::desktop_task::start(owner, sink, move |output| async move {
                    encode_result(
                        installer
                            .install(move |progress| output.emit(progress))
                            .await,
                    )
                })
            }
            Self::Search {
                query,
                max_results,
                sink,
            } => crate::desktop_task::start(owner, sink, move |_| async move {
                encode_result(browser_tools::web_search(&core.browser, query, max_results).await)
            }),
            Self::Browse {
                url,
                max_chars,
                sink,
            } => crate::desktop_task::start(owner, sink, move |_| async move {
                encode_result(browser_tools::browse_url(&core.browser, url, max_chars).await)
            }),
            Self::Screenshot { url, sink } => {
                crate::desktop_task::start(owner, sink, move |_| async move {
                    encode_result(browser_tools::browser_screenshot(&core, url).await)
                })
            }
        };
        encode_result(result)
    }
}
impl From<BrowserRequest> for DesktopRequest {
    fn from(request: BrowserRequest) -> Self {
        Self::Browser(Box::new(request))
    }
}

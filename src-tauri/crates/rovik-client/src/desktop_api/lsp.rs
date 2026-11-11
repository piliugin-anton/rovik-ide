//! Client-owned language servers and shared rust-analyzer installations.
use super::{encode_result, DesktopRequest, DesktopResult};
use crate::{desktop_backend::DesktopBackend, desktop_session::Session, lsp};
use lsp::rust_analyzer::{self as ra, RaRelease};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Language-server operations preserve the native editor contract.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum LspRequest {
    /// Starts a server and reserves its private event stream before it runs.
    Start {
        command: String,
        args: Vec<String>,
        cwd: String,
        sink: String,
    },
    /// A finite pipe write, so Stop remains available under backpressure.
    Send {
        id: String,
        message: String,
        sink: String,
    },
    /// Stops only this client's server.
    Stop { id: String },
    /// Finds a managed, custom, or system rust-analyzer.
    Find {
        custom_path: Option<String>,
        prefer_system: bool,
        sink: String,
    },
    /// Looks up the official release without holding the command connection.
    Latest { sink: String },
    /// Downloads and publishes a verified executable, reporting private progress.
    Install { release: RaRelease, sink: String },
    /// Checks the selected project's rust-src component.
    RustSrcStatus { root: String, sink: String },
    /// Installs rust-src for the selected project's toolchain.
    InstallRustSrc { root: String, sink: String },
}

impl LspRequest {
    pub(crate) async fn dispatch(
        self,
        backend: &DesktopBackend,
        session: Option<&Arc<Session>>,
    ) -> DesktopResult {
        let owner = session.ok_or_else(|| {
            serde_json::Value::String("A client session is required for language servers".into())
        })?;
        let data = backend.core.db.data_dir().to_path_buf();
        let result = match self {
            Self::Start {
                command,
                args,
                cwd,
                sink,
            } => {
                owner.begin(&sink).map_err(serde_json::Value::String)?;
                let result = lsp::start(
                    backend.lsp.clone(),
                    owner.clone(),
                    command,
                    args,
                    cwd,
                    sink.clone(),
                )
                .await;
                if result.is_err() {
                    owner.cancel_sink(&sink);
                }
                return encode_result(result);
            }
            Self::Send { id, message, sink } => {
                let server =
                    lsp::owned(&backend.lsp, owner, &id).map_err(serde_json::Value::String)?;
                crate::desktop_task::start(owner, sink, move |_| async move {
                    encode_result(lsp::send(server, message).await)
                })
            }
            Self::Stop { id } => {
                owner.remove(&format!("lsp:{id}"));
                Ok(())
            }
            Self::Find {
                custom_path,
                prefer_system,
                sink,
            } => crate::desktop_task::start(owner, sink, move |_| async move {
                encode_result(ra::ra_find(data, custom_path, prefer_system).await)
            }),
            Self::Latest { sink } => crate::desktop_task::start(owner, sink, |_| async {
                encode_result(ra::ra_latest_release().await)
            }),
            Self::Install { release, sink } => {
                crate::desktop_task::start(owner, sink, move |output| async move {
                    encode_result(
                        ra::ra_install(data, release, move |progress| output.emit(progress)).await,
                    )
                })
            }
            Self::RustSrcStatus { root, sink } => {
                crate::desktop_task::start(owner, sink, move |_| async move {
                    encode_result(ra::ra_rust_src_status(root).await)
                })
            }
            Self::InstallRustSrc { root, sink } => {
                crate::desktop_task::start(owner, sink, move |_| async move {
                    encode_result(ra::ra_install_rust_src(root).await)
                })
            }
        };
        encode_result(result)
    }
}
impl From<LspRequest> for DesktopRequest {
    fn from(request: LspRequest) -> Self {
        Self::Lsp(Box::new(request))
    }
}

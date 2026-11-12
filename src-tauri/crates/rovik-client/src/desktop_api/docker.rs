//! Local Docker setup stays in the trusted desktop backend.
use super::{encode_result, DesktopRequest, DesktopResult};
use crate::{desktop_backend::DesktopBackend, desktop_session::Session};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Docker installation and readiness, separate from cloud host operations.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum DockerRequest {
    /// Inspect the local Docker installation and Linux daemon.
    Status,
    /// Install a missing local dependency and observe progress privately.
    Install { sink: String },
}
impl DockerRequest {
    pub(crate) async fn dispatch(
        self,
        backend: &DesktopBackend,
        owner: Option<&Arc<Session>>,
    ) -> DesktopResult {
        match self {
            Self::Status => encode_result(Ok::<_, String>(crate::docker_install::status().await)),
            Self::Install { sink } => {
                let owner = owner.ok_or_else(|| {
                    serde_json::Value::String(
                        "A client session is required to install Docker".into(),
                    )
                })?;
                let installer = backend.docker_install.clone();
                let data = backend.core.db.data_dir().to_path_buf();
                encode_result(crate::desktop_task::start(
                    owner,
                    sink,
                    move |output| async move {
                        encode_result(
                            installer
                                .install(data, move |progress| output.emit(progress))
                                .await,
                        )
                    },
                ))
            }
        }
    }
}
impl From<DockerRequest> for DesktopRequest {
    fn from(request: DockerRequest) -> Self {
        Self::Docker(Box::new(request))
    }
}

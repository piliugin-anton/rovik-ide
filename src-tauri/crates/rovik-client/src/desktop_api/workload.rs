//! Workload settings are shared; focus selections belong to client sessions.
use super::{encode_result, DesktopRequest, DesktopResult};
use crate::{desktop_backend::DesktopBackend, desktop_session::Session};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Heavy-command queue controls with the existing UI reply shapes.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum WorkloadRequest {
    /// Reads shared queue capacity and effective priority.
    Status,
    /// Persists a shared manual slot limit, or restores auto.
    SetSlots { slots: Option<usize> },
    /// Promotes this client's visible chat, or releases only its own selection.
    Focus { dialog_id: Option<String> },
}
impl WorkloadRequest {
    pub(crate) async fn dispatch(
        self,
        backend: &DesktopBackend,
        owner: Option<&Arc<Session>>,
    ) -> DesktopResult {
        let owner = owner.ok_or_else(|| {
            serde_json::Value::String("A client session is required for workload commands".into())
        })?;
        match self {
            Self::Status => encode_result(backend.workload.clone().status().await),
            Self::SetSlots { slots } => {
                encode_result(backend.workload.clone().set_slots(slots).await)
            }
            Self::Focus { dialog_id } => {
                encode_result(owner.set_workload(backend.workload.clone(), dialog_id))
            }
        }
    }
}
impl From<WorkloadRequest> for DesktopRequest {
    fn from(request: WorkloadRequest) -> Self {
        Self::Workload(Box::new(request))
    }
}

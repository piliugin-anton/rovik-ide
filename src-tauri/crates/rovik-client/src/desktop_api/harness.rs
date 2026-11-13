//! Project verification routed through the chat's trusted execution owner.

use super::{encode_result, DesktopRequest, DesktopResult};
use crate::desktop_backend::DesktopBackend;
use rovik_protocol::{harness::Operation, Request, Response};
use serde::{Deserialize, Serialize};

/// Explicitly selected execution context; project paths never select a host.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionHost {
    /// Project settings on this desktop's execution backend.
    Local,
    /// The current owner of the selected chat, locally or on a cloud host.
    Chat,
}

/// A project settings or chat request from the native IDE client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessRequest {
    /// Selected local project, used only when execution is local.
    pub root: String,
    /// Explicit user-visible execution context.
    pub execution_host: ExecutionHost,
    /// The owning chat selects its actual local or cloud execution host.
    #[serde(default)]
    pub dialog_id: Option<String>,
    /// Trusted management action; the worker broker never exposes this route.
    pub request: Operation,
}

impl HarnessRequest {
    pub(crate) async fn dispatch(self, backend: &DesktopBackend) -> DesktopResult {
        if let Some(dialog_id) = self.selected_chat().map_err(serde_json::Value::String)? {
            return match backend
                .dispatch(Request::Harness {
                    dialog_id: dialog_id.to_string(),
                    root: Some(self.root),
                    source: None,
                    operation: self.request,
                })
                .await
            {
                Ok(Response::Harness(value)) => Ok(value),
                Ok(_) => Err(serde_json::Value::String(
                    "The host returned an unexpected verification reply".into(),
                )),
                Err(error) => Err(serde_json::Value::String(error.message)),
            };
        }
        let root = std::path::PathBuf::from(self.root);
        let lookup = root.clone();
        let project_id = backend
            .core
            .db
            .read(move |db| rovik_core::chat_db::projects::opened_project_at(db, &lookup))
            .await
            .map_err(serde_json::Value::String)?
            .ok_or_else(|| {
                serde_json::Value::String(
                    "Open this project in the IDE before configuring verification".into(),
                )
            })?;
        encode_result(
            rovik_core::harness::ide::execute(
                backend.core.paths.data_dir.join("harness"),
                root,
                project_id,
                self.request,
            )
            .await
            .map_err(|error| error.to_string()),
        )
    }

    fn selected_chat(&self) -> Result<Option<&str>, String> {
        match (self.execution_host, self.dialog_id.as_deref()) {
            (ExecutionHost::Local, None) => Ok(None),
            (ExecutionHost::Chat, Some(id)) if !id.trim().is_empty() => Ok(Some(id)),
            (ExecutionHost::Chat, _) => Err("Select a chat to choose its execution host".into()),
            (ExecutionHost::Local, Some(_)) => {
                Err("Local project verification cannot also select a chat execution host".into())
            }
        }
    }
}

impl From<HarnessRequest> for DesktopRequest {
    fn from(request: HarnessRequest) -> Self {
        Self::Harness(Box::new(request))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_webview_cannot_supply_source_or_state_authority() {
        let request = serde_json::json!({
            "root":"/project", "executionHost":"local", "dialogId":null,
            "request":{"action":"inspect"}
        });
        assert!(serde_json::from_value::<HarnessRequest>(request.clone()).is_ok());
        for field in ["source", "projectId", "stateRoot"] {
            let mut forged = request.clone();
            forged[field] = serde_json::json!("worker-controlled");
            assert!(serde_json::from_value::<HarnessRequest>(forged).is_err());
        }
    }

    #[test]
    fn execution_context_cannot_silently_fall_back_to_the_local_project() {
        let mut request = HarnessRequest {
            root: "/project".into(),
            execution_host: ExecutionHost::Chat,
            dialog_id: None,
            request: Operation::Inspect { untracked: vec![] },
        };
        assert!(request.selected_chat().is_err());
        request.dialog_id = Some("remote-chat".into());
        assert_eq!(request.selected_chat().unwrap(), Some("remote-chat"));
        request.execution_host = ExecutionHost::Local;
        assert!(request.selected_chat().is_err());
        request.dialog_id = None;
        assert_eq!(request.selected_chat().unwrap(), None);
    }
}

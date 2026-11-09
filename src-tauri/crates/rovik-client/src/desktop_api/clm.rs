//! Backend-owned clm operations.

use crate::desktop_backend::DesktopBackend;
use rovik_core::clm;
use serde::{Deserialize, Serialize};

/// Desktop clm operations.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum ClmRequest {
    /// clm_tool_read on the backend.
    ToolRead {
        dialog_id: String,
        path: String,
        start_line: Option<usize>,
        end_line: Option<usize>,
    },
    /// clm_tool_edit on the backend.
    ToolEdit {
        dialog_id: String,
        path: String,
        old_text: String,
        new_text: String,
        start_line: Option<usize>,
        end_line: Option<usize>,
    },
    /// clm_tool_write on the backend.
    ToolWrite {
        dialog_id: String,
        path: String,
        content: String,
    },
    /// clm_recall_hint on the backend.
    RecallHint {
        dialog_id: String,
        tool_call_id: String,
        name: String,
        arguments: String,
    },
}

impl ClmRequest {
    pub(super) async fn dispatch(self, backend: &DesktopBackend) -> super::DesktopResult {
        match self {
            Self::ToolRead {
                dialog_id,
                path,
                start_line,
                end_line,
            } => super::encode_result(
                clm_tool_read(backend, dialog_id, path, start_line, end_line).await,
            ),
            Self::ToolEdit {
                dialog_id,
                path,
                old_text,
                new_text,
                start_line,
                end_line,
            } => super::encode_result(
                clm_tool_edit(
                    backend, dialog_id, path, old_text, new_text, start_line, end_line,
                )
                .await,
            ),
            Self::ToolWrite {
                dialog_id,
                path,
                content,
            } => super::encode_result(clm_tool_write(backend, dialog_id, path, content).await),
            Self::RecallHint {
                dialog_id,
                tool_call_id,
                name,
                arguments,
            } => super::encode_result(
                clm_recall_hint(backend, dialog_id, tool_call_id, name, arguments).await,
            ),
        }
    }
}

async fn clm_tool_read(
    backend: &DesktopBackend,
    dialog_id: String,
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<String, String> {
    let core = &backend.core;
    clm::tool_read(&core.db, &core.vfs, &dialog_id, &path, start_line, end_line).await
}

async fn clm_tool_edit(
    backend: &DesktopBackend,
    dialog_id: String,
    path: String,
    old_text: String,
    new_text: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<String, String> {
    let core = &backend.core;
    clm::tool_edit(
        &core.vfs, &dialog_id, &path, &old_text, &new_text, start_line, end_line,
    )
}

async fn clm_tool_write(
    backend: &DesktopBackend,
    dialog_id: String,
    path: String,
    content: String,
) -> Result<String, String> {
    let core = &backend.core;
    clm::tool_write(&core.vfs, &dialog_id, &path, &content)
}

async fn clm_recall_hint(
    backend: &DesktopBackend,
    dialog_id: String,
    tool_call_id: String,
    name: String,
    arguments: String,
) -> Result<Option<String>, String> {
    let core = &backend.core;
    clm::recall_hint_of(&core.db, dialog_id, tool_call_id, name, arguments).await
}

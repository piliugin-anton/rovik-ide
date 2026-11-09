//! Backend-owned chat operations.

use rovik_core::agent_run::autopilot::{self, StageRow};
use rovik_core::agent_run::StartRequest;
use rovik_core::chat_db::search::SearchHit;
use rovik_core::chat_db::store::{ChatModel, Cursor, DialogPage, DialogSummary, MessagePage};
use rovik_core::chat_db::{api, projects};
use std::collections::HashMap;

use crate::desktop_backend::DesktopBackend;
use serde::{Deserialize, Serialize};

/// An attachment's validated media type and compact binary payload.
#[derive(Serialize, Deserialize)]
pub struct Attachment {
    /// Media type determined by attachment storage.
    pub mime: String,
    /// Original bytes, encoded compactly across the desktop transport.
    #[serde(with = "super::bytes")]
    pub bytes: Vec<u8>,
}

/// Desktop chat operations.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum ChatRequest {
    /// Reads a content-addressed attachment without exposing backend storage paths.
    ReadAttachment { path: String },
    /// Create a chat in the backend database.
    CreateDialog {
        id: String,
        title: String,
        created_at: i64,
        updated_at: i64,
        project_id: Option<String>,
    },
    /// Store an attachment with the existing image/PDF validation.
    StoreAttachment {
        #[serde(with = "super::bytes")]
        bytes: Vec<u8>,
    },
    /// chat_list_dialogs on the backend.
    ListDialogs {
        project_id: Option<String>,
        cursor: Option<Cursor>,
        limit: Option<usize>,
    },
    /// chat_get_dialog on the backend.
    GetDialog { dialog_id: String },
    /// chat_read_plan on the backend.
    ReadPlan { dialog_id: String, path: String },
    /// chat_plan_implementation on the backend.
    PlanImplementation {
        project_id: Option<String>,
        path: String,
        content: String,
    },
    /// chat_decline_projects on the backend.
    DeclineProjects {
        root: String,
        project_ids: Vec<String>,
    },
    /// chat_list_projects on the backend.
    ListProjects,
    /// chat_get_messages on the backend.
    GetMessages {
        dialog_id: String,
        before_seq: Option<i64>,
        limit: Option<usize>,
    },
    /// chat_task_stages on the backend.
    TaskStages { dialog_id: String },
    /// chat_get_messages_around on the backend.
    GetMessagesAround {
        message_id: String,
        before: Option<usize>,
        after: Option<usize>,
    },
    /// chat_get_message_body on the backend.
    GetMessageBody { message_id: String },
    /// cloud_move_preflight on the backend.
    MovePreflight { dialog_id: String, host: String },
    /// cloud_move_chat on the backend.
    MoveChat {
        dialog_id: String,
        host: String,
        start: Option<StartRequest>,
    },
    /// cloud_move_back on the backend.
    MoveBack {
        dialog_id: String,
        start: Option<StartRequest>,
    },
    /// cloud_cancel_move on the backend.
    CancelMove { dialog_id: String },
    /// cloud_forwarded_ports on the backend.
    ForwardedPorts,
    /// chat_resync_dialog on the backend.
    ResyncDialog { dialog_id: String },
    /// chat_search on the backend.
    Search {
        query: String,
        project_id: Option<String>,
        limit: Option<usize>,
    },
    /// chat_unsettled_sends on the backend.
    UnsettledSends { project_id: Option<String> },
    /// chat_messages_answered on the backend.
    MessagesAnswered { message_ids: Vec<String> },
    /// chat_messages_stored on the backend.
    MessagesStored { message_ids: Vec<String> },
    /// chat_save_composer_draft on the backend.
    SaveComposerDraft { dialog_id: String, content: String },
    /// chat_get_composer_draft on the backend.
    GetComposerDraft { dialog_id: String },
    /// chat_get_model on the backend.
    GetModel { dialog_id: String },
}

impl ChatRequest {
    pub(super) async fn dispatch(self, backend: &DesktopBackend) -> super::DesktopResult {
        match self {
            Self::ReadAttachment { path } => {
                let data = backend.core.db.data_dir().to_path_buf();
                super::encode_result(
                    super::blocking(move || {
                        Ok(rovik_core::chat_db::attachments::read(&data, &path).map(
                            |(mime, bytes)| Attachment {
                                mime: mime.into(),
                                bytes,
                            },
                        ))
                    })
                    .await,
                )
            }
            Self::CreateDialog {
                id,
                title,
                created_at,
                updated_at,
                project_id,
            } => super::encode_result(
                api::chat_create_dialog(
                    &backend.core.db,
                    rovik_core::chat_db::store::NewDialog {
                        id,
                        title,
                        created_at,
                        updated_at,
                        project_id,
                    },
                )
                .await,
            ),
            Self::StoreAttachment { bytes } => super::encode_result(
                rovik_core::chat_db::attachments::store_attached(&backend.core.db, bytes).await,
            ),
            Self::ListDialogs {
                project_id,
                cursor,
                limit,
            } => super::encode_result(chat_list_dialogs(backend, project_id, cursor, limit).await),
            Self::GetDialog { dialog_id } => {
                super::encode_result(chat_get_dialog(backend, dialog_id).await)
            }
            Self::ReadPlan { dialog_id, path } => {
                super::encode_result(chat_read_plan(backend, dialog_id, path).await)
            }
            Self::PlanImplementation {
                project_id,
                path,
                content,
            } => super::encode_result(
                chat_plan_implementation(backend, project_id, path, content).await,
            ),
            Self::DeclineProjects { root, project_ids } => {
                super::encode_result(chat_decline_projects(backend, root, project_ids).await)
            }
            Self::ListProjects => super::encode_result(chat_list_projects(backend).await),
            Self::GetMessages {
                dialog_id,
                before_seq,
                limit,
            } => {
                super::encode_result(chat_get_messages(backend, dialog_id, before_seq, limit).await)
            }
            Self::TaskStages { dialog_id } => {
                super::encode_result(chat_task_stages(backend, dialog_id).await)
            }
            Self::GetMessagesAround {
                message_id,
                before,
                after,
            } => super::encode_result(
                chat_get_messages_around(backend, message_id, before, after).await,
            ),
            Self::GetMessageBody { message_id } => {
                super::encode_result(chat_get_message_body(backend, message_id).await)
            }
            Self::MovePreflight { dialog_id, host } => {
                super::encode_result(cloud_move_preflight(backend, dialog_id, host).await)
            }
            Self::MoveChat {
                dialog_id,
                host,
                start,
            } => super::encode_result(cloud_move_chat(backend, dialog_id, host, start).await),
            Self::MoveBack { dialog_id, start } => {
                super::encode_result(cloud_move_back(backend, dialog_id, start).await)
            }
            Self::CancelMove { dialog_id } => {
                super::encode_result(cloud_cancel_move(backend, dialog_id).await)
            }
            Self::ForwardedPorts => super::encode_result(cloud_forwarded_ports(backend).await),
            Self::ResyncDialog { dialog_id } => {
                super::encode_result(chat_resync_dialog(backend, dialog_id).await)
            }
            Self::Search {
                query,
                project_id,
                limit,
            } => super::encode_result(chat_search(backend, query, project_id, limit).await),
            Self::UnsettledSends { project_id } => {
                super::encode_result(chat_unsettled_sends(backend, project_id).await)
            }
            Self::MessagesAnswered { message_ids } => {
                super::encode_result(chat_messages_answered(backend, message_ids).await)
            }
            Self::MessagesStored { message_ids } => {
                super::encode_result(chat_messages_stored(backend, message_ids).await)
            }
            Self::SaveComposerDraft { dialog_id, content } => {
                super::encode_result(chat_save_composer_draft(backend, dialog_id, content).await)
            }
            Self::GetComposerDraft { dialog_id } => {
                super::encode_result(chat_get_composer_draft(backend, dialog_id).await)
            }
            Self::GetModel { dialog_id } => {
                super::encode_result(chat_get_model(backend, dialog_id).await)
            }
        }
    }
}

async fn chat_list_dialogs(
    backend: &DesktopBackend,
    project_id: Option<String>,
    cursor: Option<Cursor>,
    limit: Option<usize>,
) -> Result<DialogPage, String> {
    let core = &backend.core;
    api::chat_list_dialogs(&core.db, project_id, cursor, limit).await
}

async fn chat_get_dialog(
    backend: &DesktopBackend,
    dialog_id: String,
) -> Result<Option<DialogSummary>, String> {
    let core = &backend.core;
    api::chat_get_dialog(&core.db, dialog_id).await
}

async fn chat_read_plan(
    backend: &DesktopBackend,
    dialog_id: String,
    path: String,
) -> Result<String, String> {
    let remote = &backend.remote;
    if remote.host_of(&dialog_id).is_some() {
        return remote.read_plan(&dialog_id, &path).await;
    }
    if !rovik_core::agent_run::plan::is_plan_file(&path) {
        return Err("Not a plan's file.".into());
    }
    std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))
}

async fn chat_plan_implementation(
    backend: &DesktopBackend,
    project_id: Option<String>,
    path: String,
    content: String,
) -> Result<Option<rovik_core::agent_run::plan::PlanImplementation>, String> {
    let core = &backend.core;
    core.db
        .read(move |conn| {
            rovik_core::agent_run::plan::plan_implementation(
                conn,
                project_id.as_deref(),
                &path,
                &content,
            )
        })
        .await
}

async fn chat_decline_projects(
    backend: &DesktopBackend,
    root: String,
    project_ids: Vec<String>,
) -> Result<(), String> {
    let core = &backend.core;
    api::chat_decline_projects(&core.db, root, project_ids).await
}

async fn chat_list_projects(
    backend: &DesktopBackend,
) -> Result<Vec<projects::ProjectSummary>, String> {
    let core = &backend.core;
    api::chat_list_projects(&core.db).await
}

async fn chat_get_messages(
    backend: &DesktopBackend,
    dialog_id: String,
    before_seq: Option<i64>,
    limit: Option<usize>,
) -> Result<MessagePage, String> {
    let core = &backend.core;
    api::chat_get_messages(&core.db, dialog_id, before_seq, limit).await
}

async fn chat_task_stages(
    backend: &DesktopBackend,
    dialog_id: String,
) -> Result<Option<Vec<StageRow>>, String> {
    let core = &backend.core;
    autopilot::task_stages(core, &dialog_id).await
}

async fn chat_get_messages_around(
    backend: &DesktopBackend,
    message_id: String,
    before: Option<usize>,
    after: Option<usize>,
) -> Result<MessagePage, String> {
    let core = &backend.core;
    api::chat_get_messages_around(&core.db, message_id, before, after).await
}

async fn chat_get_message_body(
    backend: &DesktopBackend,
    message_id: String,
) -> Result<String, String> {
    let core = &backend.core;
    api::chat_get_message_body(&core.db, message_id).await
}

async fn cloud_move_preflight(
    backend: &DesktopBackend,
    dialog_id: String,
    host: String,
) -> Result<crate::MovePreflight, String> {
    let remote = &backend.remote;
    remote
        .move_preflight(&dialog_id, &host)
        .await
        .map_err(|fail| fail.message)
}

async fn cloud_move_chat(
    backend: &DesktopBackend,
    dialog_id: String,
    host: String,
    start: Option<StartRequest>,
) -> Result<(), String> {
    let remote = &backend.remote;
    remote
        .move_chat(&dialog_id, &host, start)
        .await
        .map_err(|fail| fail.message)
}

async fn cloud_move_back(
    backend: &DesktopBackend,
    dialog_id: String,
    start: Option<StartRequest>,
) -> Result<(), String> {
    let remote = &backend.remote;
    remote
        .move_back(&dialog_id, start)
        .await
        .map_err(|fail| fail.message)
}

async fn cloud_cancel_move(backend: &DesktopBackend, dialog_id: String) -> Result<(), String> {
    let remote = &backend.remote;
    remote
        .cancel_move(&dialog_id)
        .await
        .map_err(|fail| fail.message)
}

async fn cloud_forwarded_ports(
    backend: &DesktopBackend,
) -> Result<HashMap<String, Vec<crate::ForwardedPort>>, String> {
    let remote = &backend.remote;
    Ok(remote.all_forwarded_ports())
}

async fn chat_resync_dialog(backend: &DesktopBackend, dialog_id: String) -> Result<(), String> {
    let remote = &backend.remote;
    remote
        .resync_chat(&dialog_id)
        .await
        .map_err(|fail| fail.message)
}

async fn chat_search(
    backend: &DesktopBackend,
    query: String,
    project_id: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<SearchHit>, String> {
    let core = &backend.core;
    api::chat_search(&core.db, query, project_id, limit).await
}

async fn chat_unsettled_sends(
    backend: &DesktopBackend,
    project_id: Option<String>,
) -> Result<Vec<rovik_core::chat_db::store::UnsettledSend>, String> {
    let core = &backend.core;
    api::chat_unsettled_sends(&core.db, project_id).await
}

async fn chat_messages_answered(
    backend: &DesktopBackend,
    message_ids: Vec<String>,
) -> Result<Vec<String>, String> {
    let core = &backend.core;
    api::chat_messages_answered(&core.db, message_ids).await
}

async fn chat_messages_stored(
    backend: &DesktopBackend,
    message_ids: Vec<String>,
) -> Result<Vec<String>, String> {
    let core = &backend.core;
    api::chat_messages_stored(&core.db, message_ids).await
}

async fn chat_save_composer_draft(
    backend: &DesktopBackend,
    dialog_id: String,
    content: String,
) -> Result<(), String> {
    let core = &backend.core;
    api::chat_save_composer_draft(&core.db, dialog_id, content).await
}

async fn chat_get_composer_draft(
    backend: &DesktopBackend,
    dialog_id: String,
) -> Result<Option<String>, String> {
    let core = &backend.core;
    api::chat_get_composer_draft(&core.db, dialog_id).await
}

async fn chat_get_model(
    backend: &DesktopBackend,
    dialog_id: String,
) -> Result<Option<ChatModel>, String> {
    let core = &backend.core;
    api::chat_get_model(&core.db, dialog_id).await
}

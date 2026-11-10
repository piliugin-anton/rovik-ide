//! Project lifecycle is serialized in the backend; native window focus stays in Tauri.
use super::{encode, encode_result, DesktopRequest, DesktopResult};
use crate::{desktop_backend::DesktopBackend, desktop_session::Session};
use rovik_core::chat_db::{api, projects};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};

/// Client-owned project registration and project/chat lifecycle operations.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum ProjectRequest {
    /// Opens and registers this client's project.
    Open { root: String },
    /// Re-registers a previously opened folder without resolving, moving, or merging history.
    Restore { root: String, project_id: String },
    /// Releases this client's previous folder when it closes or switches projects.
    Clear,
    /// Resolves a copied folder's history.
    AnswerCopy {
        root: String,
        original_id: String,
        share: bool,
    },
    /// Resolves a moved folder's history.
    AnswerMoved {
        root: String,
        current_id: String,
        target_id: String,
    },
    /// Relinks a missing project's history.
    Relink {
        project_id: String,
        root: String,
        merge: bool,
    },
    /// Deletes an idle project's chats.
    Delete { project_id: String },
    /// Chats held by the backend or another process must not have their drafts judged as abandoned.
    AtWork { dialog_ids: Vec<String> },
    /// Whether a project's chats still hold execution leases.
    AtWorkIn { project_id: Option<String> },
}

impl ProjectRequest {
    pub(super) async fn dispatch(
        self,
        backend: &DesktopBackend,
        session: Option<&Arc<Session>>,
    ) -> DesktopResult {
        encode_result(self.run(backend, session).await)
    }

    async fn run(
        self,
        backend: &DesktopBackend,
        session: Option<&Arc<Session>>,
    ) -> Result<serde_json::Value, String> {
        let owner = session.ok_or("A client session is required for project operations")?;
        if let Self::AtWork { dialog_ids } = self {
            let held = rovik_core::chat_hold::held_here();
            return encode(
                dialog_ids
                    .into_iter()
                    .filter(|id| held.contains(id) || rovik_core::chat_hold::elsewhere(id))
                    .collect::<Vec<_>>(),
            );
        }
        if let Self::AtWorkIn { project_id } = self {
            let held = rovik_core::chat_hold::held_here();
            return encode(
                backend
                    .core
                    .db
                    .read(move |c| projects::has_lead_chat(c, &held, project_id.as_deref()))
                    .await?,
            );
        }
        // Check and mutation are one turn across all clients, including opens.
        let _turn = backend.project_changes.lock().await;
        if owner.is_closed() {
            return Err("Desktop session ended; reconnect before changing projects".into());
        }
        let remote = &backend.remote;
        let guard = |id: &str, root: Option<&str>, action: &str| match remote
            .project_open_elsewhere(owner.id(), id, root.map(Path::new))
        {
            Some(folder) => Err(format!(
                "{} is open in another window: close it there first, then {action}.",
                folder.display()
            )),
            None => Ok(()),
        };
        let register =
            |id: String, root: String| owner.set_project(remote.clone(), Some((id, root.into())));
        match self {
            Self::Open { root } => {
                let id = projects::folder_project_id(Path::new(&root)).unwrap_or_default();
                guard(&id, Some(&root), "open it here")?;
                let opened = api::chat_open_project(&backend.core.db, root.clone()).await?;
                guard(&opened.project.id, Some(&root), "open it here")?;
                register(opened.project.id.clone(), root)?;
                encode(opened)
            }
            Self::Restore { root, project_id } => {
                guard(&project_id, Some(&root), "restore it here")?;
                if projects::folder_project_id(Path::new(&root)).as_deref() != Some(&project_id) {
                    return Err(
                        "Project identity changed; reopen the folder before continuing".into(),
                    );
                }
                let id = project_id.clone();
                let path = std::path::PathBuf::from(&root);
                if !backend
                    .core
                    .db
                    .read(move |c| projects::chat_may_use(c, Some(&id), &path))
                    .await?
                {
                    return Err(
                        "Project folder is no longer registered; reopen it before continuing"
                            .into(),
                    );
                }
                register(project_id, root)?;
                encode(())
            }
            Self::Clear => {
                owner.set_project(remote.clone(), None)?;
                encode(())
            }
            Self::AnswerCopy {
                root,
                original_id,
                share,
            } => {
                guard("", Some(&root), "change its chats")?;
                if share {
                    guard(&original_id, None, "share its chats")?;
                }
                let project =
                    api::chat_answer_copy(&backend.core.db, root.clone(), original_id, share)
                        .await?;
                register(project.id.clone(), root)?;
                encode(project)
            }
            Self::AnswerMoved {
                root,
                current_id,
                target_id,
            } => {
                guard(&current_id, Some(&root), "move its chats")?;
                guard(&target_id, None, "use its chats here")?;
                remote.before_merging_project(&current_id).await?;
                let project =
                    api::chat_answer_moved(&backend.core.db, root.clone(), current_id, target_id)
                        .await?;
                register(project.id.clone(), root)?;
                encode(project)
            }
            Self::Relink {
                project_id,
                root,
                merge,
            } => {
                guard(&project_id, Some(&root), "move its chats")?;
                if let Some(target) = projects::folder_project_id(Path::new(&root)) {
                    guard(&target, None, "move its chats")?;
                }
                if merge {
                    remote.before_merging_project(&project_id).await?;
                }
                let was_open = remote.client_project(owner.id()).is_some_and(|(id, path)| {
                    id == project_id
                        || projects::path_key(&path) == projects::path_key(Path::new(&root))
                });
                let result =
                    api::chat_relink_project(&backend.core.db, project_id, root.clone(), merge)
                        .await?;
                if let (true, projects::Relinked::Done { project }) = (was_open, &result) {
                    register(project.id.clone(), root)?;
                }
                encode(result)
            }
            Self::Delete { project_id } => {
                guard(&project_id, None, "delete it")?;
                remote.before_deleting_project(&project_id).await?;
                let was_open = remote
                    .client_project(owner.id())
                    .is_some_and(|(id, _)| id == project_id);
                api::chat_delete_project(&backend.core, project_id).await?;
                if was_open {
                    owner.set_project(remote.clone(), None)?;
                }
                encode(())
            }
            Self::AtWork { .. } | Self::AtWorkIn { .. } => unreachable!(),
        }
    }
}
impl From<ProjectRequest> for DesktopRequest {
    fn from(request: ProjectRequest) -> Self {
        Self::Projects(Box::new(request))
    }
}

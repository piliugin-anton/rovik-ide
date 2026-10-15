//! What the daemon does with a request: the core's operations through
//! `dispatch`, as the app does for its own chats (D3), and the Host group,
//! which only a daemon answers. The WebSocket and `POST /call` both come
//! here, so a call gets the same reply either way (4.1).

use crate::auth::Tokens;
use crate::events::Events;
use crate::host;
use crate::packs::Projects;
use crate::results::Results;
use crate::store::Store;
use crate::work;
use rovik_core::chat_db::store::{self, NewDialog};
use rovik_core::{project_profile, Core};
use rovik_protocol::agent::StartRequest;
use rovik_protocol::{Fail, FailCode, Request, Response};
use std::path::PathBuf;
use std::sync::Arc;

/// The oldest app protocol the daemon serves (4.5).
pub const MIN_PROTOCOL: u32 = 1;

pub struct Daemon {
    pub core: Core,
    /// The computers that may connect (config.toml, read at start-up).
    pub tokens: Tokens,
    /// Where the core's events go, and where clients subscribe to them.
    pub events: Arc<Events>,
    /// Each chat's computer and settings, each computer's configuration.
    pub store: Arc<Store>,
    /// Each project's repository, for the packs (6.4).
    pub projects: Projects,
    /// Where the chats' worktrees go (6.4).
    pub worktrees: PathBuf,
    /// The chats' results (6.5).
    pub results: Arc<Results>,
    /// `--same-machine`: a chat without a worktree runs in the folder its
    /// start names, for development.
    pub same_machine: bool,
    /// Why the virtual files aren't mounted, when they aren't.
    pub virtual_files_problem: Option<String>,
}

impl Daemon {
    /// `computer`'s request: the computer the token named.
    pub async fn answer(&self, computer: &str, request: Request) -> Result<Response, Fail> {
        match request {
            Request::HostInfo => Ok(Response::HostInfo(host::info(self).await)),
            Request::AgentStart(start) => self.start(computer, start).await,
            // Only this computer's configuration changes (D16).
            Request::AgentConfigure(config) => self
                .store
                .configure(computer, config)
                .map(|()| Response::Done)
                .map_err(|e| Fail::new(FailCode::Internal, e)),
            Request::ProjectEnsure {
                project,
                settings,
                excludes,
            } => work::ensure_project(self, project, settings, excludes).await,
            Request::WorkSync {
                dialog_id,
                project,
                snapshot,
                excludes,
            } => work::sync(self, computer, &dialog_id, &project, &snapshot, excludes).await,
            Request::WorkResult { dialog_id } => {
                self.store.check_owner(computer, &dialog_id)?;
                let outcome = self.store.outcome_of(&dialog_id);
                let outcome = outcome.as_deref().unwrap_or("unknown");
                let result = self.results.commit(&dialog_id, outcome).await?;
                Ok(Response::ChatResult(result))
            }
            Request::WorkResults => Ok(Response::ChatResults(self.store.results_of(computer))),
            // The chat's worktree goes with its rows.
            Request::ChatDelete { dialog_id } => {
                let delete = Request::ChatDelete {
                    dialog_id: dialog_id.clone(),
                };
                let reply = rovik_core::dispatch(&self.core, delete).await?;
                work::remove(self, &dialog_id).await;
                Ok(reply)
            }
            request => rovik_core::dispatch(&self.core, request).await,
        }
    }

    /// A start (6.2): the store keeps its keys apart and its settings, and
    /// the chat becomes the computer's.
    async fn start(&self, computer: &str, start: StartRequest) -> Result<Response, Fail> {
        self.can_run(&start.dialog_id)?;
        self.store.started(computer, &start)?;
        self.run(start).await
    }

    /// A chat runs in its worktree (6.4), or, with `--same-machine`, in the
    /// folder its start names.
    pub fn can_run(&self, dialog_id: &str) -> Result<(), Fail> {
        if self.same_machine || work::root(self, dialog_id).is_some() {
            return Ok(());
        }
        Err(Fail::new(
            FailCode::BadRequest,
            "This chat has no worktree on this server yet: its project syncs first \
             (WorkSync). A daemon started with --same-machine runs it in the folder its \
             start names instead.",
        ))
    }

    /// Starts the chat's run, which follows its computer's configuration
    /// (`config_for`), never another's.
    pub async fn run(&self, mut start: StartRequest) -> Result<Response, Fail> {
        start.config = None;
        // The daemon works in its own copy, never in an overlay: the
        // chat's worktree, whatever folder the start names (D8).
        start.isolate = Some(false);
        if let Some(root) = work::root(self, &start.dialog_id) {
            start.root = Some(work::text(&root));
        }
        self.keep_project_settings(&mut start)?;
        self.ensure_chat(&start.dialog_id, None).await?;
        rovik_core::dispatch(&self.core, Request::AgentStart(start)).await
    }

    /// The project's settings the start brought, kept for the folder its
    /// run works in (6.2), where the run reads them as the app's runs read
    /// the app's.
    fn keep_project_settings(&self, start: &mut StartRequest) -> Result<(), Fail> {
        let (Some(settings), Some(root)) = (start.project_settings.take(), &start.root) else {
            return Ok(());
        };
        let config_dir = &self.core.paths.config_dir;
        project_profile::save_project_settings(config_dir, root.clone(), settings)
            .map_err(|e| Fail::new(FailCode::Internal, e))
    }

    /// A chat started here before chats can move here (M04) has no row
    /// yet: it gets one, so its messages can be stored. A chat with a
    /// worktree belongs to the worktree's project (6.4).
    pub async fn ensure_chat(&self, dialog_id: &str, project: Option<&str>) -> Result<(), Fail> {
        let (id, project) = (dialog_id.to_string(), project.map(String::from));
        self.core
            .db
            .write(move |c| {
                if store::get_dialog(c, &id)?.is_some() {
                    return match project {
                        Some(project) => store::set_dialog_project(c, &id, Some(&project)),
                        None => Ok(()),
                    };
                }
                let now = store::now_ms();
                store::create_dialog(
                    c,
                    &NewDialog {
                        id,
                        title: "Chat".into(),
                        created_at: now,
                        updated_at: now,
                        project_id: project,
                    },
                )
            })
            .await
            .map_err(|e| Fail::new(FailCode::Internal, e))
    }
}

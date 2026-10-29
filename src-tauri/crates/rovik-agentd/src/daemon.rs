//! What the daemon does with a request: the core's operations through
//! `dispatch`, as the app does for its own chats (D3), and the Host group,
//! which only a daemon answers. The WebSocket and `POST /call` both come
//! here, so a call gets the same reply either way (4.1).

use crate::auth::Tokens;
use crate::changes;
use crate::events::Events;
use crate::gc;
use crate::host;
use crate::limits::Caps;
use crate::moves;
use crate::packs::Projects;
use crate::results::{self, Results};
use crate::store::Store;
use crate::work;
use rovik_core::agent_run::wire_content;
use rovik_core::chat_db::location::Location;
use rovik_core::chat_db::store::{self, NewDialog};
use rovik_core::overlay::chats;
use rovik_core::{project_profile, Core};
use rovik_protocol::agent::{AgentStatus, StartKind, StartRequest};
use rovik_protocol::{Access, Fail, FailCode, Request, Response, Target};
use std::collections::{BTreeSet, HashMap};
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
    /// The TMPDIR of the daemon and the agent's commands (K13).
    pub tmp: PathBuf,
    /// The chats' results (6.5).
    pub results: Arc<Results>,
    /// `--same-machine`: a chat without a worktree runs in the folder its
    /// start names, for development.
    pub same_machine: bool,
    /// Why the virtual files aren't mounted, when they aren't.
    pub virtual_files_problem: Option<String>,
    /// One chat's import or release at a time (6.8).
    pub imports: tokio::sync::Mutex<()>,
    /// A team server's caps on this user's worktrees and disk (5.5).
    pub caps: Caps,
    /// One worktree made at a time: the caps are checked, and the worktree
    /// made, under it, so two at once can't both take the last place.
    pub making: tokio::sync::Mutex<()>,
    /// The pages this user's commands asked to open, waiting for their
    /// computer's answer (D20).
    pub pages: crate::pages::Opener,
}

impl Daemon {
    /// `computer`'s request: the computer the token named. A chat another
    /// computer moved here is refused, its subagents' chats with it,
    /// whatever the request (D16, 4.2), and so is anything but a read, a
    /// release or `ChatForget` of a chat the computer took back (6.8). A
    /// request that changed a chat tells its computer (`ChatChanged`, 6.3).
    pub async fn answer(&self, computer: &str, request: Request) -> Result<Response, Fail> {
        let mut changes = None;
        if let Some((target, access)) = request.target() {
            let lead = self.lead_of(target).await?;
            self.store.check_owner(computer, &lead)?;
            let taking_back = matches!(
                request,
                Request::ChatRelease { .. } | Request::ChatForget { .. }
            );
            if access != Access::Read && !taking_back && self.store.is_released(&lead) {
                let why = rovik_core::refusal(&Location::Released, access);
                return Err(Fail::new(FailCode::NotOwner, why.unwrap_or_default()));
            }
            changes = (access == Access::Write).then_some(lead);
        }
        // Settings of a run in memory, named by its chat (no `target`, as
        // they change no rows): another computer's chat is refused all the
        // same (D16).
        if let Request::AgentSetEffort {
            dialog_id: Some(chat),
            ..
        }
        | Request::WorkloadFocus {
            dialog_id: Some(chat),
        } = &request
        {
            self.store.check_owner(computer, chat)?;
        }
        let reply = self.serve(computer, request).await;
        if let (Ok(_), Some(chat)) = (&reply, changes) {
            self.events.changed(&chat);
        }
        reply
    }

    async fn serve(&self, computer: &str, request: Request) -> Result<Response, Fail> {
        match request {
            Request::HostInfo => Ok(Response::HostInfo(host::info(self).await)),
            Request::KeysList => Ok(Response::Keys(self.store.keys())),
            Request::McpSecretsKept => Ok(Response::Count(self.store.mcp_secrets_kept())),
            // The server's keys, whichever of the user's computers brought
            // them: "Remove keys from this server" leaves none.
            Request::KeysRemove { provider } => {
                let removed = self
                    .store
                    .remove_keys(provider.as_deref())
                    .map_err(|e| Fail::new(FailCode::Internal, e))?;
                // A running MCP server keeps the secrets it started with:
                // none survives in one. A call going on one fails.
                if provider.is_none() {
                    self.core.mcp.shutdown().await;
                }
                Ok(Response::Count(removed))
            }
            Request::McpTest { server, values } => {
                let given = values.into_iter().map(|(k, v)| (k, v.0.clone())).collect();
                self.core
                    .mcp
                    .test(&server, &given, None)
                    .await
                    .map(Response::McpTested)
                    .map_err(|e| Fail::new(FailCode::Internal, e))
            }
            Request::Storage => Ok(Response::Storage(host::storage(self).await)),
            Request::CleanUp => Ok(Response::Storage(gc::now(self).await)),
            Request::PageOpened {
                id,
                local_port,
                error,
            } => {
                let outcome = match (error, local_port) {
                    (None, Some(local)) => Ok(local),
                    (Some(why), _) => Err(why),
                    (None, None) => Err("The user's computer said nothing of the page.".into()),
                };
                Ok(Response::Bool(self.pages.answered(&id, outcome)))
            }
            Request::AgentStatus => self.status(computer).await.map(Response::AgentStatus),
            Request::AgentStart(start) => self.start(computer, *start).await,
            // This computer's runs only take the effort it picked (D16).
            Request::AgentSetEffort {
                profile_id,
                options,
                dialog_id,
            } => {
                let mine: std::collections::HashSet<String> = self
                    .store
                    .chats_of(computer)
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect();
                let agents = &self.core.agents;
                let reached =
                    agents.set_effort_where(&profile_id, options, dialog_id.as_deref(), |chat| {
                        mine.contains(chat)
                    });
                Ok(Response::Count(reached))
            }
            // Only this computer's configuration changes (D16). How this
            // server connects (Settings → Network's cloud hosts' rows) and
            // the hosts its agent's work reaches (allowed-hosts-plan.md,
            // J12) are the process's: the last computer to say has them.
            Request::AgentConfigure(config) => {
                let network = config.network.clone();
                let permissions = config.permissions.clone();
                let mcp_changed = (self.store.configure(computer, *config))
                    .map_err(|e| Fail::new(FailCode::Internal, e))?;
                rovik_core::net::set_host_network(network);
                rovik_core::allowed_hosts::configure(&permissions);
                self.mcp_changed(&mcp_changed);
                Ok(Response::Done)
            }
            Request::AutonomyConfigure(config) => self
                .store
                .set_autonomy(computer, config)
                .map(|()| Response::Done)
                .map_err(|e| Fail::new(FailCode::Internal, e)),
            // H08: this computer's chats, as `status` lists them.
            Request::TaskStatus => {
                let chats = self.store.chats_of(computer);
                let listed = self.core.db.read(move |c| {
                    let all = chats.iter().map(|(id, chat)| (id, chat));
                    Ok(crate::status::chat_tasks(Some(c), all))
                });
                listed
                    .await
                    .map(Response::Tasks)
                    .map_err(|e| Fail::new(FailCode::Internal, e))
            }
            // H05: with this computer's delivery settings.
            Request::DeliveryTest { remote } => {
                let delivery = self.store.computer_autonomy(computer).delivery;
                crate::deliver::test(&delivery, &remote)
                    .await
                    .map(Response::DeliveryTested)
                    .map_err(|e| Fail::new(FailCode::BadRequest, e))
            }
            Request::ProjectEnsure {
                project,
                settings,
                excludes,
                remote,
            } => work::ensure_project(self, computer, project, settings, excludes, remote).await,
            Request::WorkSync {
                dialog_id,
                project,
                snapshot,
                excludes,
                changes,
                mark,
            } => {
                // A chat's worktree is made of its computer's project only.
                self.store.check_project_owner(computer, &project)?;
                let at = (dialog_id.as_str(), project.as_str());
                work::sync(self, computer, at, &snapshot, excludes, changes, mark).await
            }
            Request::WorkResult { dialog_id } => {
                self.store.check_owner(computer, &dialog_id)?;
                let result = self.results.recommit(&dialog_id).await?;
                Ok(Response::ChatResult(result))
            }
            Request::WorkResults => Ok(Response::ChatResults(self.store.results_of(computer))),
            Request::WorkStatus { dialog_id } => changes::status(self, computer, &dialog_id).await,
            Request::WorkFile { dialog_id, path } => {
                changes::file(self, computer, &dialog_id, &path).await
            }
            Request::WorkDiscard { dialog_id } => {
                changes::discard(self, computer, &dialog_id).await
            }
            Request::WorkConflictsToChat {
                dialog_id,
                snapshot,
                files,
            } => changes::to_chat(self, computer, &dialog_id, &snapshot, files).await,
            Request::WorkUndoMerge { dialog_id, files } => {
                changes::undo_merge(self, computer, &dialog_id, files).await
            }
            Request::WorkRecheck {
                dialog_id,
                project,
                snapshot,
                excludes,
            } => {
                self.store.check_project_owner(computer, &project)?;
                work::recheck(self, computer, &dialog_id, &project, &snapshot, excludes).await
            }
            Request::WorkLanded {
                dialog_id,
                commit,
                paths,
            } => {
                self.store.check_owner(computer, &dialog_id)?;
                self.results.landed(&dialog_id, &commit, paths).await?;
                Ok(Response::Done)
            }
            Request::ChatOwner { dialog_id } => Ok(moves::owner(self, &dialog_id)),
            Request::ChatRelease { dialog_id, move_id } => {
                moves::release(self, &dialog_id, move_id).await
            }
            Request::ChatForget { dialog_id } => moves::forget(self, &dialog_id).await,
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

    /// The lead's chat of the chat a request is about: its own, for a
    /// lead's.
    async fn lead_of(&self, target: Target<'_>) -> Result<String, Fail> {
        let (id, message) = match target {
            Target::Chat(id) => (id.to_string(), false),
            Target::Message(id) => (id.to_string(), true),
        };
        self.core
            .db
            .read(move |c| {
                let chat = match message {
                    true => store::message_dialog(c, &id)?.unwrap_or(id),
                    false => id,
                };
                store::lead_chat(c, &chat)
            })
            .await
            .map_err(|e| Fail::new(FailCode::Internal, e))
    }

    /// What runs and waits here for `computer`'s chats, and never another
    /// computer's (D16).
    async fn status(&self, computer: &str) -> Result<AgentStatus, Fail> {
        let reply = rovik_core::dispatch(&self.core, Request::AgentStatus).await?;
        let mut status = AgentStatus::try_from(reply)?;
        let named: BTreeSet<String> = (status.runs.iter().map(|r| &r.dialog_id))
            .chain(status.subagents.iter().map(|s| &s.parent_dialog_id))
            .chain(status.approvals.iter().map(|a| &a.dialog_id))
            .chain(status.questions.iter().map(|q| &q.dialog_id))
            .cloned()
            .collect();
        let leads = self
            .core
            .db
            .read(move |c| {
                let lead = |chat: String| Ok((store::lead_chat(c, &chat)?, chat));
                named
                    .into_iter()
                    .map(lead)
                    .collect::<Result<Vec<(String, String)>, String>>()
            })
            .await
            .map_err(|e| Fail::new(FailCode::Internal, e))?;
        let mine: HashMap<String, bool> = leads
            .into_iter()
            .map(|(lead, chat)| {
                let owned = self
                    .store
                    .owner(&lead)
                    .is_some_and(|owner| owner == computer);
                (chat, owned)
            })
            .collect();
        let is_mine = |chat: &String| mine.get(chat).copied().unwrap_or(false);
        status.runs.retain(|r| is_mine(&r.dialog_id));
        status.subagents.retain(|s| is_mine(&s.parent_dialog_id));
        status.approvals.retain(|a| is_mine(&a.dialog_id));
        status.questions.retain(|q| is_mine(&q.dialog_id));
        // The messages sent to its runs carry the user's words.
        status.steering.retain(|s| is_mine(&s.dialog_id));
        Ok(status)
    }

    /// A start (6.2): the store keeps its keys apart and its settings, and
    /// the chat becomes the computer's.
    async fn start(&self, computer: &str, mut start: StartRequest) -> Result<Response, Fail> {
        self.can_run(&start.dialog_id)?;
        let mcp_changed = self.store.started(computer, &start)?;
        self.mcp_changed(&mcp_changed);
        self.tell_marked(&mut start);
        self.run(start).await
    }

    /// The computers' MCP servers, or their secrets, changed (Q11). A
    /// running server keeps the secrets it started with: one whose secrets
    /// changed starts again with the new ones at its next use, and one no
    /// computer has on any more stops, so none goes on with what the user
    /// took back.
    fn mcp_changed(&self, secrets_changed: &[String]) {
        for id in secrets_changed {
            self.core.mcp.forget(id);
        }
        self.core.mcp.configure(&self.store.mcp_servers());
    }

    /// The first message after a sync put conflict markers in the chat's
    /// files tells its model of them, once (6.7).
    fn tell_marked(&self, start: &mut StartRequest) {
        let StartKind::Send { message } = &start.start else {
            return;
        };
        let Some(files) = self.store.marks_to_tell(&start.dialog_id) else {
            return;
        };
        // Named as the chat sees them, from its root.
        let files = results::in_root(&self.store.subpath_of(&start.dialog_id), files);
        if files.is_empty() {
            return;
        }
        let text = (start.current_user_content.take()).unwrap_or_else(|| wire_content(message));
        let note = chats::markers_note(&files);
        start.current_user_content = Some(format!("{text}\n\nNote: {note}"));
    }

    /// Starts the chat with the checks on `files`, which the project's
    /// newer changes were merged into (6.6), with what it changed in each
    /// (`diffs`): its last start's settings, as a restart takes them.
    pub async fn check_merged(
        &self,
        dialog_id: &str,
        files: Vec<String>,
        diffs: Vec<String>,
    ) -> Result<(), Fail> {
        let start = StartKind::CheckMerged { files, diffs };
        let request = self
            .store
            .restart_request(dialog_id, start)
            .ok_or_else(|| {
                Fail::new(
                    FailCode::BadRequest,
                    "This chat never ran here: there's nothing to check.",
                )
            })?;
        self.run(request).await.map(drop)
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
    /// (`config_for`), never another's. Never a chat its computer took
    /// back (6.8), whatever starts it: a request, a restart or a task.
    pub async fn run(&self, mut start: StartRequest) -> Result<Response, Fail> {
        if self.store.is_released(&start.dialog_id) {
            let why = rovik_core::refusal(&Location::Released, Access::Write);
            return Err(Fail::new(FailCode::NotOwner, why.unwrap_or_default()));
        }
        start.config = None;
        // The daemon works in its own copy, never in an overlay: the
        // chat's worktree, whatever folder the start names (D8).
        start.isolate = Some(false);
        if let Some(root) = work::root(self, &start.dialog_id) {
            start.root = Some(work::text(&root));
        }
        self.keep_project_settings(&mut start)?;
        self.ensure_chat(&start.dialog_id, None).await?;
        rovik_core::dispatch(&self.core, Request::AgentStart(Box::new(start))).await
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

    /// A chat started here without a move (6.8) has no row yet: it gets
    /// one, so its messages can be stored. A chat with a worktree belongs
    /// to the worktree's project (6.4).
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

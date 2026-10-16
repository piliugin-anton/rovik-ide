//! The app's remote chats (6.1): which host each one is on, the requests
//! that go there, and its events, passed on to the app's own sink as the
//! frontend expects them, with the mirror written first. The mirror pulls
//! the rows as their host has them (remote/mirror.rs, 6.3). Which host a
//! chat is on is kept in the database (`dialogs.location`, `mirror_state`),
//! so the next start of the app knows it (`load`); `dispatch` refuses such
//! a chat, whose rows here only the mirror writes (K21).

use crate::client::{Client, HostState, Identity, Incoming};
use crate::connector::Connector;
use crate::hosts::Hosts;
use git2::Oid;
use results::Job;
use rovik_core::agent_run::events::{self, AgentEvent, ApprovalRequest};
use rovik_core::agent_run::subagent::Kind;
use rovik_core::agent_run::{self, AgentConfig, AgentStatus, StartKind, StartRequest, UserMessage};
use rovik_core::chat_db::location::{self, Location};
use rovik_core::chat_db::store::{self, NewChildDialog, NewMessage};
use rovik_core::chat_db::{api, attachments, moves};
use rovik_core::{project_profile, Core};
use rovik_protocol::packs::{self, Pushed};
use rovik_protocol::permissions::Approver;
use rovik_protocol::work::ChatSync;
use rovik_protocol::{features, Access, Event, Fail, FailCode, Request, Response};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

mod back;
mod bar;
mod mirror;
mod moving;
mod results;
mod sync;

pub use bar::LOCAL_ONLY;
pub use moving::MovePreflight;

/// The outcome told for a run that ended while its host was away: the
/// client can't know it until results come (T05).
const ENDED_AWAY: &str = "unknown";

/// What the app tells the frontend about its hosts on `cloud-event`,
/// apart from the chats' own events.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum CloudEvent {
    /// A message's files going up to its host before it's sent (6.11).
    Uploading {
        dialog_id: String,
        message_id: String,
        sent: u64,
        total: u64,
    },
    /// A chat's project going up to its host (6.4): the bytes of its pack
    /// sent so far. A pack's size isn't known until it's built.
    Pushing {
        dialog_id: String,
        project: String,
        sent: u64,
    },
    /// A remote chat's changes may look different: its result came in or
    /// landed, or its project synced. Its isolation bar looks again.
    Changed { dialog_id: String },
    /// A chat's move to a host (6.8): the step it's at ("pausing",
    /// "pushing", "uploading", "importing"), then "moved", "failed" with
    /// why, or "cancelled".
    Moving {
        dialog_id: String,
        step: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// A chat's move back from its host (6.8): the step it's at
    /// ("pausing", "fetching", "releasing", "pulling", "landing"), then
    /// "moved", "failed" with why, or "cancelled".
    MovingBack {
        dialog_id: String,
        step: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// A host's connection changed: the hosts page and its chats' badges
    /// show it.
    Host { host: String, state: HostState },
    /// The chats mirrored from a host have pulled since it connected: only
    /// now can a message sent before a restart be judged unanswered (K23).
    CaughtUp { host: String },
}

/// Where `CloudEvent`s go.
pub type CloudSink = Arc<dyn Fn(CloudEvent) + Send + Sync>;

/// Who approves on a host while nobody is there (6.9).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalPolicy {
    /// What would wait for the user goes to the AI reviewer first.
    AiReviewerThenMe,
    /// It waits for the user.
    #[default]
    WaitForMe,
}

impl ApprovalPolicy {
    /// `config` as the host runs this computer's chats with it.
    pub fn apply(self, config: &mut AgentConfig) {
        if self == ApprovalPolicy::WaitForMe || config.approval_profiles.is_empty() {
            return;
        }
        let p = &mut config.permissions;
        for rule in [
            &mut p.command_approval,
            &mut p.edit_approval,
            &mut p.tool_approval,
        ] {
            if rule.required && rule.by == Approver::User {
                rule.by = Approver::Ai;
            }
        }
    }
}

pub struct Remote {
    core: Core,
    hosts: Hosts,
    me: Weak<Remote>,
    state: Mutex<State>,
    cloud: Mutex<Option<CloudSink>>,
    /// The queue of results to import and land (remote/results.rs).
    jobs: mpsc::UnboundedSender<Job>,
}

#[derive(Default)]
struct State {
    /// Each remote chat's host, and its subagents' chats': the chats the
    /// database puts on a host (`location`).
    chats: HashMap<String, String>,
    policies: HashMap<String, ApprovalPolicy>,
    /// The remote runs the frontend was told of: a host that comes back
    /// without one ended it meanwhile.
    running: HashMap<String, String>,
    /// Each mirrored chat's pulls (remote/mirror.rs).
    pulls: HashMap<String, mirror::Pulls>,
    /// The chats whose move there or back goes now, and those of them
    /// asked to cancel (remote/moving.rs, remote/back.rs).
    moving: HashSet<String>,
    cancels: HashSet<String>,
    /// The chats moving back, their subagents' chats with them, by their
    /// lead's chat: what would change them is refused.
    backs: HashMap<String, String>,
    /// The sends whose files are going up, by chat: Stop cancels one.
    uploads: HashMap<String, oneshot::Sender<()>>,
    /// The project open in the app: its id and folder (6.5).
    open: Option<(String, PathBuf)>,
    /// Each chat's snapshot as its last sync took it (6.7).
    last_sync: HashMap<String, Oid>,
    /// The projects ensured on each host this session: (host, project).
    ensured: HashSet<(String, String)>,
    /// Each chat's last sync, as its host told it.
    syncs: HashMap<String, ChatSync>,
    /// How many times each chat's result went back to be checked merged
    /// with the project's newer changes (6.6), since its last message.
    rechecks: HashMap<String, u32>,
}

impl Remote {
    pub fn new(core: Core) -> Arc<Remote> {
        let (jobs, queue) = mpsc::unbounded_channel();
        let remote = Arc::new_cyclic(|me| Remote {
            core,
            hosts: Hosts::default(),
            me: me.clone(),
            state: Mutex::new(State::default()),
            cloud: Mutex::new(None),
            jobs,
        });
        rovik_core::rt::spawn(results::work_through(Arc::downgrade(&remote), queue));
        remote
    }

    /// Where uploads' and pushes' progress goes from now on.
    pub fn on_cloud(&self, sink: CloudSink) {
        *self.cloud.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink);
    }

    fn cloud(&self) -> Option<CloudSink> {
        self.cloud.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The chat's isolation bar is to look again.
    fn changed(&self, dialog_id: &str) {
        if let Some(cloud) = self.cloud() {
            cloud(CloudEvent::Changed {
                dialog_id: dialog_id.to_string(),
            });
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Connects to `host`, in place of an earlier connection to it. Its
    /// events are taken one after another, so each waits for the mirror
    /// writes of those before it. On the current Tokio runtime.
    pub fn connect(&self, host: &str, connector: impl Connector, identity: Identity) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = Arc::new(move |incoming| {
            let _ = tx.send(incoming);
        });
        self.hosts.connect(host, connector, identity, sink);
        let (me, host) = (self.me.clone(), host.to_string());
        tokio::spawn(async move {
            while let Some(incoming) = rx.recv().await {
                let Some(remote) = me.upgrade() else { break };
                remote.receive(&host, incoming).await;
            }
        });
    }

    /// The chats the database puts on hosts, once the app starts: their
    /// requests go there from now on.
    pub async fn load(&self) -> Result<(), String> {
        let away = self.core.db.read(location::away).await?;
        let mut state = self.lock();
        for (chat, location) in away {
            if let Location::Host(host) = location {
                state.chats.insert(chat, host);
            }
        }
        Ok(())
    }

    /// The chat is on `host` from now on, with its subagents' chats: here,
    /// and in the database, queued before any write that comes after.
    pub fn place(&self, dialog_id: &str, host: &str) {
        self.lock()
            .chats
            .insert(dialog_id.to_string(), host.to_string());
        let (chat, host) = (dialog_id.to_string(), host.to_string());
        self.core
            .db
            .write_detached(move |c| rovik_core::chat_db::mirror::place(c, &chat, &host));
    }

    pub fn host_of(&self, dialog_id: &str) -> Option<String> {
        self.lock().chats.get(dialog_id).cloned()
    }

    /// Each host's connection, by its id.
    pub fn states(&self) -> Vec<(String, HostState)> {
        self.hosts.states()
    }

    pub fn set_policy(&self, host: &str, policy: ApprovalPolicy) {
        self.lock().policies.insert(host.to_string(), policy);
    }

    /// How many of the user's chats are on `host` or moving to it.
    pub async fn chats_on(&self, host: &str) -> Result<usize, String> {
        let host = host.to_string();
        self.core
            .db
            .read(move |c| location::chats_on(c, &host))
            .await
    }

    /// Forgets the host (removed from the hosts page): its connection
    /// closes once no call holds it.
    pub fn disconnect(&self, host: &str) {
        self.hosts.disconnect(host);
        self.lock().policies.remove(host);
    }

    /// A call about the host itself (the Host group): `HostInfo`,
    /// `KeysList`, `KeysRemove`, `Storage`.
    pub async fn host_call(&self, host: &str, request: Request) -> Result<Response, Fail> {
        self.client(host)?.call(request).await
    }

    /// Whether the host's daemon serves `feature` (4.5): false until it
    /// has said, in its `Welcome`.
    pub fn serves(&self, host: &str, feature: &str) -> bool {
        let daemon = self.hosts.client(host).and_then(|c| c.daemon());
        daemon.is_some_and(|d| d.features.iter().any(|f| f == feature))
    }

    fn policy(&self, host: &str) -> ApprovalPolicy {
        self.lock().policies.get(host).copied().unwrap_or_default()
    }

    fn client(&self, host: &str) -> Result<Client, Fail> {
        self.hosts
            .client(host)
            .ok_or_else(|| Fail::new(FailCode::Unreachable, format!("{host} isn't set up.")))
    }

    /*
     * Requests
     */

    /// A chat's request, answered by its owner: the core here, or its host.
    /// The mirror takes the writes its owner tells no event of.
    pub async fn route(&self, dialog_id: &str, request: Request) -> Result<Response, Fail> {
        let Some(host) = self.host_of(dialog_id) else {
            return rovik_core::dispatch(&self.core, request).await;
        };
        // What the run waits on is still answered (6.8).
        let writes = matches!(request.target(), Some((_, Access::Write)));
        if writes && self.is_moving_back(dialog_id) {
            return Err(Fail::new(FailCode::BadRequest, back::MOVING_BACK));
        }
        // Stop during a send's upload cancels the send (6.11).
        if matches!(request, Request::AgentStop { .. }) {
            if let Some(cancel) = self.lock().uploads.remove(dialog_id) {
                let _ = cancel.send(());
                return Ok(Response::Bool(true));
            }
        }
        let reply = self.client(&host)?.call(request.clone()).await?;
        if let Err(e) = self.mirror_write(request).await {
            log::warn!("the mirror of {dialog_id} didn't take a write: {e}");
        }
        Ok(match reply {
            // A path on the server means nothing here.
            Response::ClmStatus(mut status) => {
                status.real_path = None;
                Response::ClmStatus(status)
            }
            reply => reply,
        })
    }

    /// A write the owner took, into the mirror at once: the pull its
    /// `ChatChanged` brings then has the owner's row (6.3). Written here
    /// directly: `dispatch` refuses a chat on a host.
    async fn mirror_write(&self, request: Request) -> Result<(), String> {
        let db = &self.core.db;
        match request {
            Request::ChatRename { dialog_id, title } => {
                api::chat_rename_dialog(db, dialog_id, title).await
            }
            Request::ChatSetModel { dialog_id, model } => {
                api::chat_set_model(db, dialog_id, model).await
            }
            Request::ChatUpdateMessage { message_id, patch } => {
                api::chat_update_message(db, message_id, patch).await
            }
            _ => Ok(()),
        }
    }

    /// Deletes a chat (6.1). A remote chat's rows here go at once, and its
    /// host deletes its own, its run stopped and its worktree gone: now, or
    /// when it's next connected, from the tombstone written first. A chat
    /// moving to a host is refused, as `dispatch` refuses it, and so is one
    /// moving back.
    pub async fn delete(&self, dialog_id: &str) -> Result<(), Fail> {
        let Some(host) = self.host_of(dialog_id) else {
            let delete = Request::ChatDelete {
                dialog_id: dialog_id.to_string(),
            };
            return rovik_core::dispatch(&self.core, delete).await.map(drop);
        };
        if self.is_moving_back(dialog_id) {
            return Err(Fail::new(FailCode::BadRequest, back::MOVING_BACK));
        }
        let (chat, on) = (dialog_id.to_string(), host.clone());
        let id = uuid::Uuid::new_v4().to_string();
        let buried = self.core.db.write(move |c| {
            moves::bury(c, &chat, &on, &id)?;
            let mut chats = store::descendants(c, &chat)?;
            chats.push(chat);
            Ok(chats)
        });
        let chats = buried.await.map_err(internal)?;
        api::chat_delete_dialog(&self.core, dialog_id.to_string())
            .await
            .map_err(internal)?;
        {
            let mut state = self.lock();
            for chat in &chats {
                state.chats.remove(chat);
                state.running.remove(chat);
                state.pulls.remove(chat);
                state.last_sync.remove(chat);
                state.syncs.remove(chat);
                state.rechecks.remove(chat);
            }
        }
        if let Some(client) = self.hosts.client(&host) {
            self.delete_there(&client, dialog_id).await;
        }
        Ok(())
    }

    /// A request about a message, answered by the owner of its chat, which
    /// the local database knows.
    pub async fn route_message(
        &self,
        message_id: &str,
        request: Request,
    ) -> Result<Response, Fail> {
        let id = message_id.to_string();
        let dialog = self
            .core
            .db
            .read(move |c| store::message_dialog(c, &id))
            .await
            .map_err(internal)?;
        match dialog {
            Some(dialog) => self.route(&dialog, request).await,
            None => rovik_core::dispatch(&self.core, request).await,
        }
    }

    /// A start (6.1). A remote chat's is prepared for its host first: its
    /// project checked here and synced before a message (6.7), local paths
    /// taken out of the request's text (K08), the project's settings added
    /// (K03), the host's approval policy applied (6.9), and the text left
    /// out when the host has it already (K50).
    pub async fn start(&self, mut request: StartRequest) -> Result<Response, Fail> {
        let Some(host) = self.host_of(&request.dialog_id) else {
            return rovik_core::dispatch(&self.core, Request::AgentStart(request)).await;
        };
        if self.is_moving_back(&request.dialog_id) {
            return Err(Fail::new(FailCode::BadRequest, back::MOVING_BACK));
        }
        let client = self.client(&host)?;
        agent_run::check_project(&self.core.db, &request.dialog_id, request.root.as_deref())
            .await
            .map_err(internal)?;
        if matches!(request.start, StartKind::Send { .. }) {
            self.sync_before_send(&host, &request.dialog_id).await;
        }
        if let Some(root) = request.root.clone().filter(|r| !r.trim().is_empty()) {
            relative_paths(&mut request, &root);
            let settings =
                project_profile::load_project_settings(&self.core.paths.config_dir, &root);
            request.project_settings = Some(settings).filter(|s| !s.is_null());
        }
        if let Some(config) = &mut request.config {
            self.policy(&host).apply(config);
        }
        self.leave_out_known_text(&mut request).await;
        if let StartKind::Send { message } = &mut request.start {
            if !message.attachments.is_empty() {
                self.send_files(&host, &client, &request.dialog_id, message)
                    .await?;
            }
        }
        client.call(Request::AgentStart(request)).await
    }

    /// A start other than `Send` sends the request's text only when it
    /// isn't what the host has already: its last request as stored
    /// (`meta.sentContent`, else the text). A Continue doesn't send a
    /// message's text files again (K50).
    async fn leave_out_known_text(&self, request: &mut StartRequest) {
        if matches!(request.start, StartKind::Send { .. }) {
            return;
        }
        let Some(current) = request.current_user_content.clone() else {
            return;
        };
        let id = request.dialog_id.clone();
        let stored = self
            .core
            .db
            .read(move |c| store::get_messages(c, &id, None, 500))
            .await
            .map(|page| agent_run::last_user_wire_content(&page.messages));
        if matches!(stored, Ok(Some(text)) if text == current) {
            request.current_user_content = None;
        }
    }

    /// A message's files go up before it (6.11): checked here as the host
    /// would, kept here from cleanup (sending cleared the draft that kept
    /// them), and those the host lacks uploaded, with progress on
    /// `cloud-event`. Stop cancels it, and the message isn't sent.
    async fn send_files(
        &self,
        host: &str,
        client: &Client,
        dialog_id: &str,
        message: &mut UserMessage,
    ) -> Result<(), Fail> {
        let serves = |d: crate::DaemonInfo| d.features.iter().any(|f| f == features::ATTACHMENTS);
        if client.daemon().is_some_and(|d| !serves(d)) {
            return Err(Fail::new(
                FailCode::Version,
                format!(
                    "{host} runs a Rovik too old to take attached files. Upgrade it, or send \
                     the message without them."
                ),
            ));
        }
        agent_run::check_attachments(&self.core.db, message)
            .await
            .map_err(internal)?;
        let ids = message.attachments.clone();
        let renewed = ids.clone();
        self.core
            .db
            .write(move |c| attachments::renew(c, &renewed))
            .await
            .map_err(internal)?;
        let (cancel, cancelled) = oneshot::channel();
        self.lock().uploads.insert(dialog_id.to_string(), cancel);
        let uploaded = tokio::select! {
            uploaded = self.upload(client, dialog_id, &message.id, ids) => uploaded,
            Ok(()) = cancelled => Err(Fail::new(
                FailCode::Internal,
                "Stopped before the message's files were sent.",
            )),
        };
        self.lock().uploads.remove(dialog_id);
        uploaded
    }

    async fn upload(
        &self,
        client: &Client,
        dialog_id: &str,
        message_id: &str,
        ids: Vec<String>,
    ) -> Result<(), Fail> {
        let missing: Vec<String> = client.request(Request::BlobsMissing { ids }).await?;
        let data_dir = self.core.db.data_dir().to_path_buf();
        let mut files = Vec::with_capacity(missing.len());
        for id in missing {
            let bytes = attachments::file_for(&data_dir, &id)
                .and_then(|path| std::fs::read(path).ok())
                .ok_or_else(|| Fail::new(FailCode::BadRequest, agent_run::ATTACHMENT_GONE))?;
            files.push((id, bytes));
        }
        let total: u64 = files.iter().map(|(_, bytes)| bytes.len() as u64).sum();
        let mut done = 0;
        for (id, bytes) in files {
            let len = bytes.len() as u64;
            let (cloud, chat, message) =
                (self.cloud(), dialog_id.to_string(), message_id.to_string());
            let progress = move |sent: u64| {
                if let Some(cloud) = &cloud {
                    cloud(CloudEvent::Uploading {
                        dialog_id: chat.clone(),
                        message_id: message.clone(),
                        sent: done + sent,
                        total,
                    });
                }
            };
            client.put_blob(&id, bytes, progress).await?;
            done += len;
        }
        Ok(())
    }

    /// Sends the chat's snapshot to its host, as the chat's snapshot ref
    /// there (6.4), with the pack's progress on `cloud-event`.
    pub async fn push_snapshot(
        &self,
        dialog_id: &str,
        project: &str,
        shadow: &Path,
        snapshot: Oid,
    ) -> Result<Pushed, Fail> {
        let host = self
            .host_of(dialog_id)
            .ok_or_else(|| Fail::new(FailCode::BadRequest, "The chat isn't on a host."))?;
        self.push_to(&host, dialog_id, project, shadow, snapshot)
            .await
    }

    /// `push_snapshot` to `host`, where the chat is or goes.
    pub(super) async fn push_to(
        &self,
        host: &str,
        dialog_id: &str,
        project: &str,
        shadow: &Path,
        snapshot: Oid,
    ) -> Result<Pushed, Fail> {
        let (cloud, chat, project_id) = (self.cloud(), dialog_id.to_string(), project.to_string());
        let progress = move |sent| {
            if let Some(cloud) = &cloud {
                cloud(CloudEvent::Pushing {
                    dialog_id: chat.clone(),
                    project: project_id.clone(),
                    sent,
                });
            }
        };
        let name = packs::snapshot_ref(dialog_id);
        let client = self.client(host)?;
        client
            .push(project, shadow, &name, snapshot, progress)
            .await
    }

    /// What runs and waits, here and on every connected host (6.1).
    pub async fn status(&self) -> Result<AgentStatus, Fail> {
        let reply = rovik_core::dispatch(&self.core, Request::AgentStatus).await?;
        let mut status = AgentStatus::try_from(reply)?;
        for (host, client) in self.hosts.ready() {
            match client.request::<AgentStatus>(Request::AgentStatus).await {
                Ok(theirs) => {
                    status.runs.extend(theirs.runs);
                    status.subagents.extend(theirs.subagents);
                    status
                        .approvals
                        .extend(theirs.approvals.into_iter().map(mark_remote));
                    status.questions.extend(theirs.questions);
                }
                Err(fail) => log::warn!("{host}'s status: {fail}"),
            }
        }
        Ok(status)
    }

    /// A setting every run follows, applied here and sent to every
    /// connected host (6.1). A configuration goes without its keys, which
    /// the host has from the starts (D11), and under the host's policy.
    pub async fn everywhere(&self, request: Request) -> Result<Response, Fail> {
        let reply = rovik_core::dispatch(&self.core, request.clone()).await?;
        for (host, client) in self.hosts.ready() {
            let mut request = request.clone();
            if let Request::AgentConfigure(config) = &mut request {
                *config = config.without_keys();
                self.policy(&host).apply(config);
            }
            if let Err(fail) = client.call(request).await {
                log::warn!("{host} didn't take a setting: {fail}");
            }
        }
        Ok(reply)
    }

    /// Refused while the project has chats on a host, or moving to one:
    /// merging or deleting it would rewrite or orphan them (K38). `doing`
    /// ends the sentence: "deleting it", "merging it".
    pub async fn keep_project(&self, project_id: &str, doing: &str) -> Result<(), String> {
        let id = project_id.to_string();
        let away = self
            .core
            .db
            .read(move |c| location::project_has_chats_away(c, &id))
            .await?;
        match away {
            Some(host) => Err(format!(
                "This project has chats on {host}. Move them back to this computer before {doing}."
            )),
            None => Ok(()),
        }
    }

    /*
     * Events
     */

    async fn receive(&self, host: &str, incoming: Incoming) {
        match incoming {
            Incoming::State(state) => {
                log::info!("{host}: {state:?}");
                if let Some(cloud) = self.cloud() {
                    let host = host.to_string();
                    cloud(CloudEvent::Host { host, state });
                }
            }
            Incoming::Subscribed { complete } => {
                if !complete {
                    self.catch_up(host).await;
                }
                self.after_connecting(host).await;
                self.pull_all(host).await;
                self.finish_moves(host).await;
            }
            Incoming::Event { event, .. } => match event {
                Event::Agent(event) => self.agent_event(host, event).await,
                Event::ChatChanged { dialog_id, rev } => {
                    self.chat_changed(host, &dialog_id, rev).await
                }
                Event::Resync { dialog_id } => self.resync(host, &dialog_id).await,
                Event::ResultReady(result) => self.result_ready(host, result),
                Event::SyncState(sync) => {
                    if self.host_of(&sync.dialog_id).as_deref() == Some(host) {
                        self.synced(sync);
                    }
                }
                Event::HostNotice { kind, message } => self.core.emit(AgentEvent::Notice {
                    dialog_id: None,
                    kind,
                    title: host.to_string(),
                    message,
                }),
            },
        }
    }

    /// A remote chat's event, passed on as 6.1 says.
    async fn agent_event(&self, host: &str, mut event: AgentEvent) {
        // A subagent's chat goes with its lead's.
        if let AgentEvent::SubagentStarted {
            dialog_id,
            child_dialog_id,
            ..
        } = &event
        {
            if self.host_of(dialog_id).as_deref() == Some(host) {
                self.place(child_dialog_id, host);
            }
        }
        // The daemon sends only this computer's chats: a second guard.
        if let Some(dialog) = event.dialog_id() {
            if self.host_of(dialog).as_deref() != Some(host) {
                return;
            }
        }
        match &mut event {
            // A path on the server: the project changes here when the
            // result lands, which the file watcher sees (4.1).
            AgentEvent::FileWritten { .. } => return,
            AgentEvent::MessagesAdded {
                dialog_id,
                messages,
            } => {
                // In first, at their owner's numbers, with their files:
                // the next pull completes them (6.1).
                let files = messages.iter().flat_map(|m| m.attachments.iter().cloned());
                self.fetch_files(host, files.collect()).await;
                let rows: Vec<(i64, NewMessage)> = messages
                    .iter()
                    .map(|m| (m.seq, events::stored_row(m)))
                    .collect();
                let id = dialog_id.clone();
                let written = self
                    .core
                    .db
                    .write(move |c| store::append_numbered(c, &id, &rows))
                    .await;
                if let Err(e) = written {
                    log::warn!("the mirror of {dialog_id}: {e}");
                }
            }
            AgentEvent::SubagentStarted {
                dialog_id,
                tool_call_id,
                child_dialog_id,
                kind,
                goal,
                ..
            } => {
                let child = NewChildDialog {
                    id: child_dialog_id.clone(),
                    parent_id: dialog_id.clone(),
                    parent_call_id: tool_call_id.clone(),
                    kind: kind.clone(),
                    title: Kind::parse(kind)
                        .map(|k| k.chat_title(goal))
                        .unwrap_or_else(|| goal.clone()),
                    created_at: store::now_ms(),
                };
                let made = self
                    .core
                    .db
                    .write(move |c| match store::get_dialog(c, &child.id)? {
                        Some(_) => Ok(()),
                        None => store::create_child_dialog(c, &child),
                    })
                    .await;
                if let Err(e) = made {
                    log::warn!("the mirror of {child_dialog_id}: {e}");
                }
            }
            AgentEvent::Notice { title, .. } => *title = format!("{host}: {title}"),
            AgentEvent::ApprovalRequested { request } => {
                *request = mark_remote(request.clone());
            }
            AgentEvent::ContextUpdated { status, .. } => status.real_path = None,
            AgentEvent::RunStarted { dialog_id, run_id } => {
                self.lock()
                    .running
                    .insert(dialog_id.clone(), run_id.clone());
            }
            AgentEvent::RunEnded { dialog_id, .. } => {
                self.lock().running.remove(dialog_id);
                self.pull_later(host, dialog_id, Duration::ZERO);
            }
            _ => {}
        }
        self.core.emit(event);
    }

    /// Back after missing events (4.3): the frontend is told what runs and
    /// waits on the host now, and of the runs that ended meanwhile.
    async fn catch_up(&self, host: &str) {
        let Some(mut status) = self.status_of(host).await else {
            return;
        };
        let runs = std::mem::take(&mut status.runs);
        let now: HashSet<&str> = runs.iter().map(|r| r.dialog_id.as_str()).collect();
        let ended: Vec<(String, String)> = self
            .lock()
            .running
            .iter()
            .filter(|(chat, _)| !now.contains(chat.as_str()))
            .map(|(chat, run)| (chat.clone(), run.clone()))
            .collect();
        for (dialog_id, run_id) in ended {
            if self.host_of(&dialog_id).as_deref() == Some(host) {
                let outcome = ENDED_AWAY.to_string();
                let event = AgentEvent::RunEnded {
                    dialog_id,
                    run_id,
                    outcome,
                };
                self.agent_event(host, event).await;
            }
        }
        for run in runs {
            let event = AgentEvent::RunStarted {
                dialog_id: run.dialog_id,
                run_id: run.run_id,
            };
            self.agent_event(host, event).await;
        }
        self.waiting(host, status, None).await;
    }

    /// In place of an event too big for a frame (4.1): the chat's rows,
    /// pulled, which the frontend then hears of, and what waits in it, from
    /// the host's status.
    async fn resync(&self, host: &str, dialog_id: &str) {
        if self.host_of(dialog_id).as_deref() != Some(host) {
            return;
        }
        self.pull_later(host, dialog_id, Duration::ZERO);
        if let Some(status) = self.status_of(host).await {
            self.waiting(host, status, Some(dialog_id)).await;
        }
    }

    async fn status_of(&self, host: &str) -> Option<AgentStatus> {
        let client = self.hosts.client(host)?;
        match client.request::<AgentStatus>(Request::AgentStatus).await {
            Ok(status) => Some(status),
            Err(fail) => {
                log::warn!("{host}'s status: {fail}");
                None
            }
        }
    }

    /// The approvals and questions that wait in the status, all or the
    /// chat's, told again: the frontend keeps one of each.
    async fn waiting(&self, host: &str, status: AgentStatus, chat: Option<&str>) {
        let wanted = |dialog: &str| chat.is_none_or(|c| c == dialog);
        for request in status.approvals {
            if wanted(&request.dialog_id) {
                self.agent_event(host, AgentEvent::ApprovalRequested { request })
                    .await;
            }
        }
        for request in status.questions {
            if wanted(&request.dialog_id) {
                self.agent_event(host, AgentEvent::QuestionsAsked { request })
                    .await;
            }
        }
    }
}

/// Whether the host takes projects (6.4): a daemon from before doesn't.
fn takes_projects(client: &Client) -> bool {
    let projects = |d: crate::DaemonInfo| d.features.iter().any(|f| f == features::PROJECTS);
    client.daemon().is_some_and(projects)
}

/// The editor context names files from the project's root, not by a local
/// path that means nothing on the server (K08).
fn relative_paths(request: &mut StartRequest, root: &str) {
    let relative = |text: &str| -> String {
        let root = root.trim_end_matches(['/', '\\']);
        text.replace(&format!("{root}/"), "")
            .replace(&format!("{root}\\"), "")
    };
    if let Some(text) = &mut request.current_user_content {
        *text = relative(text);
    }
    if let StartKind::Send { message } = &mut request.start {
        let sent = message.meta.as_mut().and_then(|m| m.get_mut("sentContent"));
        if let Some(serde_json::Value::String(text)) = sent {
            *text = relative(text);
        }
    }
}

/// An edit on the server is shown apart from the local file (K09).
fn mark_remote(mut request: ApprovalRequest) -> ApprovalRequest {
    request.remote = request.edit.is_some();
    request
}

fn internal(message: String) -> Fail {
    Fail::new(FailCode::Internal, message)
}

#[cfg(test)]
mod tests;

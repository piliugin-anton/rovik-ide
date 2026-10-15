//! The app's remote chats (6.1): which host each one is on, the requests
//! that go there, and its events, passed on to the app's own sink as the
//! frontend expects them, with the mirror written first.

use crate::client::{Client, HostState, Identity, Incoming};
use crate::connector::Connector;
use crate::hosts::Hosts;
use git2::Oid;
use results::Job;
use rovik_core::agent_run::events::{self, AgentEvent, ApprovalRequest, OutMessage, Patch};
use rovik_core::agent_run::subagent::Kind;
use rovik_core::agent_run::{self, AgentConfig, AgentStatus, StartKind, StartRequest, UserMessage};
use rovik_core::chat_db::attachments;
use rovik_core::chat_db::store::{self, MessagePatch, NewChildDialog, NewMessage};
use rovik_core::overlay::import::Imported;
use rovik_core::{project_profile, Core};
use rovik_protocol::packs::{self, Pushed};
use rovik_protocol::permissions::Approver;
use rovik_protocol::work::ChatSync;
use rovik_protocol::{features, Event, Fail, FailCode, Request, Response};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use tokio::sync::{mpsc, oneshot};

mod bar;
mod results;
mod sync;

pub use bar::LOCAL_ONLY;

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
}

/// Where `CloudEvent`s go.
pub type CloudSink = Arc<dyn Fn(CloudEvent) + Send + Sync>;

/// Who approves on a host while nobody is there (6.9).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
    /// Each remote chat's host, and its subagents' chats' (`location`
    /// once M01 adds it).
    chats: HashMap<String, String>,
    policies: HashMap<String, ApprovalPolicy>,
    /// The remote runs the frontend was told of: a host that comes back
    /// without one ended it meanwhile.
    running: HashMap<String, String>,
    /// Messages sent to a host and not yet announced by it, by id, with
    /// their chats: a `Resync` in place of the announcement brings them
    /// to the mirror.
    sent: HashMap<String, (String, NewMessage)>,
    /// The sends whose files are going up, by chat: Stop cancels one.
    uploads: HashMap<String, oneshot::Sender<()>>,
    /// The project open in the app: its id and folder (6.5).
    open: Option<(String, PathBuf)>,
    /// Landings their hosts weren't told of yet, by chat.
    unreported: HashMap<String, Imported>,
    /// Each chat's snapshot as its last sync took it (6.7).
    last_sync: HashMap<String, Oid>,
    /// The projects ensured on each host this session: (host, project).
    ensured: HashSet<(String, String)>,
    /// Each chat's last sync, as its host told it.
    syncs: HashMap<String, ChatSync>,
    /// How many times each chat's result went back to be checked merged
    /// with the project's newer changes (6.6), since its last message.
    rechecks: HashMap<String, u32>,
    /// The number of each chat's newest result handled this session (6.5):
    /// an older one, replayed after it, is passed over. M01 keeps it in
    /// `mirror_state`.
    results_seen: HashMap<String, u64>,
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

    /// The chat is on `host` from now on.
    pub fn place(&self, dialog_id: &str, host: &str) {
        self.lock()
            .chats
            .insert(dialog_id.to_string(), host.to_string());
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
        // Stop during a send's upload cancels the send (6.11).
        if matches!(request, Request::AgentStop { .. }) {
            if let Some(cancel) = self.lock().uploads.remove(dialog_id) {
                let _ = cancel.send(());
                return Ok(Response::Bool(true));
            }
        }
        let reply = self.client(&host)?.call(request.clone()).await?;
        if mirror_takes(&request) {
            if let Err(fail) = rovik_core::dispatch(&self.core, request).await {
                log::warn!("the mirror of {dialog_id} didn't take a write: {fail}");
            }
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
        let sent = match &request.start {
            StartKind::Send { message } => {
                let row = (
                    request.dialog_id.clone(),
                    agent_run::stored_message(message),
                );
                self.lock().sent.insert(message.id.clone(), row);
                Some(message.id.clone())
            }
            _ => None,
        };
        let reply = client.call(Request::AgentStart(request)).await;
        if let (Err(_), Some(id)) = (&reply, sent) {
            // Never stored there: the frontend shows it failed.
            self.lock().sent.remove(&id);
        }
        reply
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
        let client = self.client(&host)?;
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

    /// Refused while the project has chats on a host: merging or deleting
    /// it would rewrite or orphan them (K38). `doing` ends the sentence:
    /// "deleting it", "merging it".
    pub async fn keep_project(&self, project_id: &str, doing: &str) -> Result<(), String> {
        let id = project_id.to_string();
        let chats = self
            .core
            .db
            .read(move |c| store::dialogs_in_project(c, Some(&id)))
            .await?;
        let state = self.lock();
        match chats.iter().find_map(|chat| state.chats.get(chat)) {
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
            Incoming::State(state) => log::info!("{host}: {state:?}"),
            Incoming::Subscribed { complete } => {
                if !complete {
                    self.catch_up(host).await;
                }
                self.after_connecting(host).await;
            }
            Incoming::Event { event, .. } => match event {
                Event::Agent(event) => self.agent_event(host, event).await,
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
                {
                    let mut state = self.lock();
                    for m in messages.iter() {
                        state.sent.remove(&m.id);
                    }
                }
                self.fetch_files(host, messages).await;
                let rows: Vec<NewMessage> = messages.iter().map(events::stored_row).collect();
                match self.mirror(dialog_id, rows).await {
                    // The frontend pages by the mirror's numbers.
                    Ok(seqs) => messages
                        .iter_mut()
                        .zip(seqs)
                        .for_each(|(m, seq)| m.seq = seq),
                    Err(e) => log::warn!("the mirror of {dialog_id}: {e}"),
                }
            }
            AgentEvent::MessagePatched {
                dialog_id,
                message_id,
                patch,
            } => {
                // Until M05's pulls, the mirror follows the patches too.
                let (id, patch) = (message_id.clone(), stored_patch(patch));
                let written = self
                    .core
                    .db
                    .write(move |c| store::update_message(c, &id, &patch))
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
            }
            _ => {}
        }
        self.core.emit(event);
    }

    /// The files the rows name that aren't here, such as a screenshot a
    /// run took on the host, fetched before the rows are written: a row
    /// can't get its files later (6.1). One the host can't send gets a row
    /// without a file, and the rows go in all the same (K44).
    async fn fetch_files(&self, host: &str, messages: &[OutMessage]) {
        let named: BTreeSet<String> = messages
            .iter()
            .flat_map(|m| m.attachments.iter().cloned())
            .collect();
        if named.is_empty() {
            return;
        }
        let ids: Vec<String> = named.into_iter().collect();
        let data_dir = self.core.db.data_dir().to_path_buf();
        let asked = ids.clone();
        let missing = self
            .core
            .db
            .read(move |c| attachments::missing_files(c, &data_dir, &asked))
            .await
            .unwrap_or(ids);
        let client = self.hosts.client(host);
        for id in missing {
            let fetched = match &client {
                Some(client) => client.blob(&id).await.ok().flatten(),
                None => None,
            };
            let stored = match fetched {
                Some(bytes) if attachments::id_of(&bytes) == id => {
                    match attachments::attachment_mime(&bytes) {
                        Some(mime) => attachments::store(&self.core.db, bytes, mime).await.is_ok(),
                        None => false,
                    }
                }
                _ => false,
            };
            if !stored {
                log::warn!("{host} couldn't send the file {id}");
                let id = id.clone();
                let added = self
                    .core
                    .db
                    .write(move |c| attachments::add_unavailable(c, &id))
                    .await;
                if let Err(e) = added {
                    log::warn!("the mirror's attachments: {e}");
                }
            }
        }
    }

    /// Rows into the mirror, those it lacks: their `seq` there.
    async fn mirror(&self, dialog_id: &str, rows: Vec<NewMessage>) -> Result<Vec<i64>, String> {
        let id = dialog_id.to_string();
        self.core
            .db
            .write(move |c| store::append_missing(c, &id, &rows))
            .await
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

    /// In place of an event too big for a frame (4.1): what waits in the
    /// chat, from the host's status, and the messages this computer sent
    /// it that weren't announced. Rows only the host has wait for the
    /// mirror's pull (M05).
    async fn resync(&self, host: &str, dialog_id: &str) {
        if self.host_of(dialog_id).as_deref() != Some(host) {
            return;
        }
        let rows: Vec<NewMessage> = {
            let mut state = self.lock();
            let ids: Vec<String> = state
                .sent
                .iter()
                .filter(|(_, (chat, _))| chat == dialog_id)
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter()
                .filter_map(|id| state.sent.remove(id))
                .map(|(_, row)| row)
                .collect()
        };
        if !rows.is_empty() {
            match self.mirror(dialog_id, rows.clone()).await {
                Ok(seqs) => {
                    let messages: Vec<OutMessage> = rows
                        .iter()
                        .zip(seqs)
                        .map(|(row, seq)| events::out_message(dialog_id, seq, row, false))
                        .collect();
                    self.core.emit(AgentEvent::MessagesAdded {
                        dialog_id: dialog_id.to_string(),
                        messages,
                    });
                }
                Err(e) => log::warn!("the mirror of {dialog_id}: {e}"),
            }
        }
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

/// Writes the owner tells no event of: the mirror takes them too, once the
/// owner has, until M05's pulls bring them.
fn mirror_takes(request: &Request) -> bool {
    matches!(
        request,
        Request::ChatRename { .. }
            | Request::ChatSetModel { .. }
            | Request::ChatUpdateMessage { .. }
    )
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

/// A streamed change as the mirror stores it.
fn stored_patch(patch: &Patch) -> MessagePatch {
    MessagePatch {
        content: patch.content.clone(),
        tool_calls: patch.tool_calls.clone(),
        meta: patch.meta.clone(),
        error: patch.error.clone(),
        failed: patch.failed,
        finalize: patch.finalized == Some(true),
    }
}

fn internal(message: String) -> Fail {
    Fail::new(FailCode::Internal, message)
}

#[cfg(test)]
mod tests;

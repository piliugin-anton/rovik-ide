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
use rovik_protocol::autopilot::{AutonomyConfig, AutonomySettings, DeliveryTest};
use rovik_protocol::network::HostNetwork;
use rovik_protocol::packs::{self, Pushed};
use rovik_protocol::permissions::Approver;
use rovik_protocol::work::{ChatResult, ChatSync};
use rovik_protocol::{features, Access, Event, Fail, FailCode, Request, Response};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// How long a host has to answer what every host is asked (its status, a
/// setting): one that stalls can't hold the others up.
const HOSTS_WAIT: Duration = Duration::from_secs(15);

mod back;
mod bar;
mod mirror;
mod moving;
mod pages;
mod results;
mod sync;
mod tab_port;

pub use bar::LOCAL_ONLY;
pub use moving::MovePreflight;
pub use pages::{ForwardedPort, OpenPage, PageToOpen};
pub use sync::HandedSync;

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
    /// show it. A host that turned this computer away says why.
    Host {
        host: String,
        state: HostState,
        #[serde(skip_serializing_if = "Option::is_none")]
        problem: Option<String>,
    },
    /// The chats mirrored from a host have pulled since it connected: only
    /// now can a message sent before a restart be judged unanswered (K23).
    CaughtUp { host: String },
    /// The ports forwarded for a chat's pages changed (D20, remote/pages.rs):
    /// each port its servers opened on its host that a page was opened
    /// from, with the port of this computer that leads there. Empty once
    /// none is forwarded.
    Ports {
        dialog_id: String,
        ports: Vec<ForwardedPort>,
    },
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
    /// "Works on its own" (autopilot-plan 3.5, H01): chats there run in
    /// Autopilot, the host's `AutonomySettings` over the computer's.
    Autonomous,
}

impl ApprovalPolicy {
    /// `config` as the host runs this computer's chats with it. A host that
    /// works on its own (H02) also has its effort and budget, each where it
    /// sets one, over the computer's (`autonomy`, its block); the rest of
    /// what an Autopilot task follows (U01's rules, A07's kinds) its core
    /// applies to every Autopilot run, there as here (`run_config`). What
    /// would wait for the user goes to the AI reviewer first, as for "Ask
    /// the AI reviewer first, then me"; in Autopilot, what it doesn't
    /// approve is refused (U02).
    pub fn apply(self, autonomy: &AutonomySettings, config: &mut AgentConfig) {
        if self == ApprovalPolicy::Autonomous {
            let autopilot = &mut config.autopilot;
            if let Some(effort) = autonomy.effort {
                autopilot.effort = effort;
            }
            if let Some(budget) = &autonomy.budget {
                let kept = &mut autopilot.budget;
                kept.max_hours = budget.max_hours.or(kept.max_hours);
                kept.max_cost = budget.max_cost.or(kept.max_cost);
                kept.max_tokens = budget.max_tokens.or(kept.max_tokens);
            }
            if let Some(plan_fix) = &autonomy.plan_fix {
                plan_fix.apply(&mut autopilot.plan_fix);
            }
        }
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
    /// Whether a project is open in another of Rovik's windows, each a
    /// process with its own connections (the app's instances.rs).
    elsewhere: Mutex<Option<OpenElsewhere>>,
    /// What brings the hosts' policies up to date before they go to a
    /// host: another window may have changed them (HostsPage::follow).
    refresh: Mutex<Option<Refresh>>,
    /// The ports forwarded for the chats' pages (remote/pages.rs).
    pages: pages::Pages,
}

/// Whether the project with this id is open in another of Rovik's windows:
/// that window lands its chats' results and shows their events.
pub type OpenElsewhere = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Brings the hosts' policies up to date from their settings file.
pub type Refresh = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct State {
    /// Each remote chat's host, and its subagents' chats': the chats the
    /// database puts on a host (`location`).
    chats: HashMap<String, String>,
    policies: HashMap<String, ApprovalPolicy>,
    /// Each host's block for when it works on its own (H01), whatever its
    /// policy: it applies under `Autonomous` only.
    autonomy: HashMap<String, AutonomySettings>,
    /// The remote runs the frontend was told of: a host that comes back
    /// without one ended it meanwhile.
    running: HashMap<String, String>,
    /// The remote chats whose Autopilot driver the frontend was told is at
    /// work (`TaskDriving`): one a host comes back without is done.
    driving: HashSet<String>,
    /// Each mirrored chat's pulls (remote/mirror.rs).
    pulls: HashMap<String, mirror::Pulls>,
    /// The chats whose move there or back goes now, each held for this
    /// process meanwhile (chat_hold.rs), and those of them asked to cancel
    /// (remote/moving.rs, remote/back.rs).
    moving: HashMap<String, rovik_core::chat_hold::Hold>,
    cancels: HashSet<String>,
    /// The chats moving back, their subagents' chats with them, by their
    /// lead's chat: what would change them is refused.
    backs: HashMap<String, String>,
    /// The sends whose files are going up, by chat: Stop cancels one.
    uploads: HashMap<String, oneshot::Sender<()>>,
    /// The messages sent to a run whose files are going up (steering), by
    /// message id, with their chat: Stop cancels them, and stops the run.
    steer_uploads: HashMap<String, (String, oneshot::Sender<()>)>,
    /// The host this computer's chats run on when they don't run here:
    /// its Linux side (the WSL distribution, on Windows), where their
    /// commands are sandboxed. A chat of a project moves there before its
    /// first start (remote/moving.rs, `go_home`).
    home: Option<String>,
    /// The projects open in the app's window, each by its id and folder:
    /// its folders' (a workspace's are several), as the window's claims
    /// record them (the app's instances.rs; workspaces-plan.md, 3.8).
    /// Results land by themselves only in them (6.5).
    open: Vec<(String, PathBuf)>,
    /// Each chat's snapshot as its last sync took it (6.7).
    last_sync: HashMap<String, Oid>,
    /// The projects ensured on each host this session: (host, project).
    ensured: HashSet<(String, String)>,
    /// Each chat's last sync, as its host told it.
    syncs: HashMap<String, ChatSync>,
    /// The chats whose waiting sync the user let be (Keep the older copy):
    /// the bar leaves it out until the next sync.
    kept: HashSet<String>,
    /// Each chat's newer result held back while its conflicts are resolved
    /// (6.5), with its host: the bar offers it in their place.
    held: HashMap<String, (String, ChatResult)>,
    /// The hosts whose mirrored chats pulled since they connected (K23):
    /// a window that missed `CaughtUp` asks.
    caught_up: HashSet<String>,
    /// Each remote chat's project, as looked up (None: it has none).
    projects: HashMap<String, Option<String>>,
    /// Whether each project was open in another window, as last asked: a
    /// run's events come many a second.
    elsewhere_seen: HashMap<String, (std::time::Instant, bool)>,
    /// How many times each chat's result went back to be checked merged
    /// with the project's newer changes (6.6), since its last message.
    rechecks: HashMap<String, u32>,
    /// Whether a configuration was applied this session (`everywhere`): a
    /// host connecting after that is sent it (`configure`).
    configured: bool,
}

impl State {
    /// Forgets what was known of `chat` on its host: it left for this
    /// computer, or was deleted there.
    fn forget(&mut self, chat: &str) {
        self.chats.remove(chat);
        self.running.remove(chat);
        self.pulls.remove(chat);
        self.last_sync.remove(chat);
        self.syncs.remove(chat);
        self.rechecks.remove(chat);
    }
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
            elsewhere: Mutex::new(None),
            refresh: Mutex::new(None),
            pages: pages::Pages::default(),
        });
        rovik_core::rt::spawn(results::work_through(Arc::downgrade(&remote), queue));
        remote
    }

    /// Where uploads' and pushes' progress goes from now on.
    /// The app's data folder.
    pub(crate) fn data_dir(&self) -> &Path {
        &self.core.paths.data_dir
    }

    pub fn on_cloud(&self, sink: CloudSink) {
        *self.cloud.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink);
    }

    /// How to tell a project open in another of Rovik's windows.
    pub fn on_open_elsewhere(&self, elsewhere: OpenElsewhere) {
        *self.elsewhere.lock().unwrap_or_else(|e| e.into_inner()) = Some(elsewhere);
    }

    /// How to bring the hosts' policies up to date before one is sent.
    pub fn on_refresh(&self, refresh: Refresh) {
        *self.refresh.lock().unwrap_or_else(|e| e.into_inner()) = Some(refresh);
    }

    fn refresh_policies(&self) {
        let refresh = self
            .refresh
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(refresh) = refresh {
            refresh();
        }
    }

    /// Whether the remote chat is another window's: its project is open
    /// there, and not here. That window imports and lands its results,
    /// pulls its rows and shows its events (approvals, questions): one
    /// window does, as one did when Rovik was a single process. A chat
    /// whose project is open nowhere is every window's.
    pub async fn handled_elsewhere(&self, dialog_id: &str) -> bool {
        let Some(project) = self.chat_project(dialog_id).await else {
            return false;
        };
        if self.is_open(&project) {
            return false;
        }
        if let Some((at, seen)) = self.lock().elsewhere_seen.get(&project) {
            if at.elapsed() < Duration::from_secs(2) {
                return *seen;
            }
        }
        let elsewhere = self
            .elsewhere
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let seen = elsewhere.is_some_and(|elsewhere| elsewhere(&project));
        self.lock()
            .elsewhere_seen
            .insert(project, (std::time::Instant::now(), seen));
        seen
    }

    /// The chat's project, looked up once.
    async fn chat_project(&self, dialog_id: &str) -> Option<String> {
        if let Some(known) = self.lock().projects.get(dialog_id) {
            return known.clone();
        }
        let id = dialog_id.to_string();
        let project = self
            .core
            .db
            .read(move |c| store::dialog_project(c, &id))
            .await
            .ok()?;
        self.lock()
            .projects
            .insert(dialog_id.to_string(), project.clone());
        project
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

    /// The host this computer's chats go to before they run, when they
    /// have one (`State::home`): none from now on with `None`.
    pub fn set_home(&self, host: Option<&str>) {
        self.lock().home = host.map(str::to_string);
    }

    pub fn home(&self) -> Option<String> {
        self.lock().home.clone()
    }

    /// Each host's connection, by its id.
    pub fn states(&self) -> Vec<(String, HostState)> {
        self.hosts.states()
    }

    /// Why the host turned this computer away, when it did.
    pub fn refusal(&self, host: &str) -> Option<String> {
        self.hosts.refusal(host)
    }

    /// Whether the chats mirrored from `host` pulled since it connected, as
    /// `CaughtUp` told (K23).
    pub fn caught_up(&self, host: &str) -> bool {
        self.lock().caught_up.contains(host)
    }

    /// The host's policy. Its projects go again with the next sync, whose
    /// `ProjectEnsure` tells a host that pushes its results where (H04).
    pub fn set_policy(&self, host: &str, policy: ApprovalPolicy) {
        let mut state = self.lock();
        state.policies.insert(host.to_string(), policy);
        state.ensured.retain(|(ensured, _)| ensured != host);
    }

    /// The host's block for when it works on its own (H01), its projects
    /// going again as for `set_policy`.
    pub fn set_autonomy(&self, host: &str, autonomy: AutonomySettings) {
        let mut state = self.lock();
        state.autonomy.insert(host.to_string(), autonomy);
        state.ensured.retain(|(ensured, _)| ensured != host);
    }

    /// "Test delivery" (H05): the host pushes an empty branch to the remote
    /// of the first open folder whose repository has an `origin` one, as a
    /// task's result would go, and deletes it.
    pub async fn test_delivery(&self, host: &str) -> Result<DeliveryTest, String> {
        let folders: Vec<PathBuf> = self.lock().open.iter().map(|(_, at)| at.clone()).collect();
        let remote = folders
            .iter()
            .find_map(|folder| {
                sync::origin_of(folder)
                    .as_deref()
                    .and_then(rovik_protocol::autopilot::push_url)
            })
            .ok_or(
                "Open a project whose repository has an `origin` remote: the test pushes there.",
            )?;
        if !self.serves(host, features::AUTONOMY) {
            return Err("This server's Rovik is too old to push results: upgrade it first.".into());
        }
        let tested = self
            .host_call(host, Request::DeliveryTest { remote })
            .await
            .map_err(|fail| fail.message)?;
        DeliveryTest::try_from(tested).map_err(|fail| fail.message)
    }

    /// After the host's policy or block changed: what follows from them
    /// goes to it now, when it's connected (`configure`), not at its next
    /// start. A host that isn't gets it on connecting.
    pub async fn reconfigure(&self, host: &str) {
        if self.hosts.ready().iter().any(|(ready, _)| ready == host) {
            self.configure(host).await;
        }
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
        self.stop_forwarding(host);
        let mut state = self.lock();
        state.policies.remove(host);
        state.autonomy.remove(host);
        if state.home.as_deref() == Some(host) {
            state.home = None;
        }
    }

    /// A call about the host itself (the Host group): `HostInfo`,
    /// `KeysList`, `KeysRemove`, `Storage`.
    pub async fn host_call(&self, host: &str, request: Request) -> Result<Response, Fail> {
        self.client(host)?.call(request).await
    }

    /// `host_call`, its answer awaited for up to `timeout`: `CleanUp`, say,
    /// which may take minutes.
    pub async fn host_call_within(
        &self,
        host: &str,
        request: Request,
        timeout: Duration,
    ) -> Result<Response, Fail> {
        self.client(host)?.call_within(request, timeout).await
    }

    /// Whether the host's daemon serves `feature` (4.5): false until it
    /// has said, in its `Welcome`.
    pub fn serves(&self, host: &str, feature: &str) -> bool {
        let daemon = self.hosts.client(host).and_then(|c| c.daemon());
        daemon.is_some_and(|d| d.serves(feature))
    }

    /// What the host's daemon serves beyond protocol 1 (4.5), as its
    /// `Welcome` said: none until it has.
    pub fn features(&self, host: &str) -> Vec<String> {
        let daemon = self.hosts.client(host).and_then(|c| c.daemon());
        daemon.map(|d| d.features).unwrap_or_default()
    }

    pub(crate) fn policy(&self, host: &str) -> ApprovalPolicy {
        self.lock().policies.get(host).copied().unwrap_or_default()
    }

    /// `config` under the host's policy, and its block when it works on its
    /// own (`ApprovalPolicy::apply`).
    /// `config`, as `host` takes it: without this computer's keys, which
    /// the host has from the starts (D11), under the host's policy, and
    /// with how its daemon connects (`network`).
    fn config_for(
        &self,
        host: &str,
        config: &AgentConfig,
        network: &Option<HostNetwork>,
    ) -> AgentConfig {
        let mut config = config.without_keys();
        self.apply_policy(host, &mut config);
        config.network = network.clone();
        config
    }

    fn apply_policy(&self, host: &str, config: &mut AgentConfig) {
        let (policy, autonomy) = {
            let state = self.lock();
            let policy = state.policies.get(host).copied().unwrap_or_default();
            (
                policy,
                state.autonomy.get(host).cloned().unwrap_or_default(),
            )
        };
        policy.apply(&autonomy, config);
    }

    /// What the host keeps for its runs that go on with this computer away
    /// (H02): its block's delivery and notify URL when it works on its own;
    /// else the default, which forgets what it had.
    pub(super) fn autonomy_config(&self, host: &str) -> AutonomyConfig {
        let state = self.lock();
        match state.policies.get(host) {
            Some(ApprovalPolicy::Autonomous) => state
                .autonomy
                .get(host)
                .map(AutonomySettings::config)
                .unwrap_or_default(),
            _ => AutonomyConfig::default(),
        }
    }

    fn client(&self, host: &str) -> Result<Client, Fail> {
        self.hosts
            .client(host)
            .ok_or_else(|| Fail::new(FailCode::Unreachable, format!("{host} isn't set up.")))
    }

    /// What the user calls `host`, for what they read: the name its
    /// settings give it, its id when it isn't set up.
    fn host_name(&self, host: &str) -> String {
        self.hosts
            .client(host)
            .map_or_else(|| host.to_string(), |client| client.name())
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
        // Stop during a send's upload cancels the send (6.11): no run goes
        // yet. One during a steer's (a message to a run that goes) cancels
        // that too, and still stops the run.
        if matches!(request, Request::AgentStop { .. }) {
            self.cancel_steer_uploads(dialog_id);
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
                state.forget(chat);
            }
        }
        self.release_pages(&chats);
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

    /// A start (6.1). A local chat of a project goes to the computer's home
    /// host first, when it has one (`go_home`). A remote chat's start is
    /// prepared for its host: its project checked here and synced before a
    /// message (6.7), local paths taken out of the request's text (K08),
    /// the project's settings added (K03), the host's approval policy
    /// applied (6.9), and the text left out when the host has it already
    /// (K50).
    pub async fn start(&self, mut request: StartRequest) -> Result<Response, Fail> {
        if self.host_of(&request.dialog_id).is_none() {
            self.go_home(&request.dialog_id).await?;
        }
        let Some(host) = self.host_of(&request.dialog_id) else {
            return rovik_core::dispatch(&self.core, Request::AgentStart(Box::new(request))).await;
        };
        if self.is_moving_back(&request.dialog_id) {
            return Err(Fail::new(FailCode::BadRequest, back::MOVING_BACK));
        }
        let client = self.client(&host)?;
        agent_run::check_project(
            &self.core.db,
            &request.dialog_id,
            request.root.as_deref(),
            &[],
        )
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
        // A chat of a project runs in its worktree there, which the daemon
        // names: no local path goes to it, a drive letter least of all (R04).
        // A daemon without projects runs in the folder the start names.
        let id = request.dialog_id.clone();
        let project = self.core.db.read(move |c| store::dialog_project(c, &id));
        if project.await.map_err(internal)?.is_some() && takes_projects(&client) {
            request.root = None;
        }
        if let Some(config) = &mut request.config {
            self.apply_policy(&host, config);
        }
        // A chat on a host that works on its own runs in Autopilot unless
        // its message names another mode (H02).
        if self.policy(&host) == ApprovalPolicy::Autonomous && request.mode.is_none() {
            request.mode = Some(agent_run::autopilot::MODE.into());
        }
        self.leave_out_known_text(&mut request).await;
        if let StartKind::Send { message } = &mut request.start {
            if !message.attachments.is_empty() {
                self.files_up(&host, &client, &request.dialog_id, message, Upload::Send)
                    .await?;
            }
        }
        client.call(Request::AgentStart(Box::new(request))).await
    }

    /// A message for the chat's run while it works (steering-plan.md, 3.8).
    /// A chat here goes to `dispatch`. For a chat on a host, the editor
    /// context's local paths are written from the chat's own folder (its
    /// project's here, workspaces-plan.md 3.8), as a start's are (K08), and
    /// its files go up first, in an upload of its own that Stop cancels.
    /// Then the host's run takes it.
    pub async fn steer(&self, dialog_id: &str, mut message: UserMessage) -> Result<Response, Fail> {
        let Some(host) = self.host_of(dialog_id) else {
            let request = Request::AgentSteer {
                dialog_id: dialog_id.to_string(),
                message,
            };
            return rovik_core::dispatch(&self.core, request).await;
        };
        let client = self.client(&host)?;
        let root = self
            .project_of(dialog_id)
            .await
            .map_err(internal)?
            .map(|(_, folder)| folder);
        if let Some(root) = root.as_deref().and_then(Path::to_str) {
            let sent = message.meta.as_mut().and_then(|m| m.get_mut("sentContent"));
            if let Some(serde_json::Value::String(text)) = sent {
                *text = relative_to(text, root);
            }
        }
        if !message.attachments.is_empty() {
            self.files_up(&host, &client, dialog_id, &mut message, Upload::Steer)
                .await?;
        }
        let request = Request::AgentSteer {
            dialog_id: dialog_id.to_string(),
            message,
        };
        self.route(dialog_id, request).await
    }

    /// Takes back a message the chat's run hasn't read yet.
    pub async fn unsteer(&self, dialog_id: &str, message_id: String) -> Result<Response, Fail> {
        let request = Request::AgentUnsteer {
            dialog_id: dialog_id.to_string(),
            message_id,
        };
        self.route(dialog_id, request).await
    }

    /// Cancels the uploads of the messages sent to the chat's run.
    fn cancel_steer_uploads(&self, dialog_id: &str) {
        let mut state = self.lock();
        let ids: Vec<String> = state
            .steer_uploads
            .iter()
            .filter(|(_, (chat, _))| chat == dialog_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some((_, cancel)) = state.steer_uploads.remove(&id) {
                let _ = cancel.send(());
            }
        }
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
    async fn files_up(
        &self,
        host: &str,
        client: &Client,
        dialog_id: &str,
        message: &mut UserMessage,
        kind: Upload,
    ) -> Result<(), Fail> {
        if client
            .daemon()
            .is_some_and(|d| !d.serves(features::ATTACHMENTS))
        {
            return Err(Fail::new(
                FailCode::Version,
                format!(
                    "{} runs a Rovik too old to take attached files. Upgrade it, or send \
                     the message without them.",
                    self.host_name(host)
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
        match kind {
            Upload::Send => {
                self.lock().uploads.insert(dialog_id.to_string(), cancel);
            }
            Upload::Steer => {
                let chat = dialog_id.to_string();
                self.lock()
                    .steer_uploads
                    .insert(message.id.clone(), (chat, cancel));
            }
        }
        let uploaded = tokio::select! {
            uploaded = self.upload(client, dialog_id, &message.id, ids) => uploaded,
            Ok(()) = cancelled => Err(Fail::new(
                FailCode::Internal,
                "Stopped before the message's files were sent.",
            )),
        };
        match kind {
            Upload::Send => {
                self.lock().uploads.remove(dialog_id);
            }
            Upload::Steer => {
                self.lock().steer_uploads.remove(&message.id);
            }
        }
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

    /// What runs and waits, here and on every connected host (6.1). The
    /// hosts are asked side by side, and a host that doesn't answer in
    /// `HOSTS_WAIT` is left out, so one that stalls holds up neither the
    /// others nor the frontend's start.
    pub async fn status(&self) -> Result<AgentStatus, Fail> {
        let reply = rovik_core::dispatch(&self.core, Request::AgentStatus).await?;
        let mut status = AgentStatus::try_from(reply)?;
        let asked = self
            .hosts
            .ready()
            .into_iter()
            .map(|(host, client)| async move {
                let theirs = client
                    .call_within(Request::AgentStatus, HOSTS_WAIT)
                    .await
                    .and_then(AgentStatus::try_from);
                (host, theirs)
            });
        for (host, theirs) in futures_util::future::join_all(asked).await {
            match theirs {
                Ok(theirs) => {
                    status.runs.extend(theirs.runs);
                    status.subagents.extend(theirs.subagents);
                    status
                        .approvals
                        .extend(theirs.approvals.into_iter().map(mark_remote));
                    status.questions.extend(theirs.questions);
                    status.driving.extend(theirs.driving);
                    status.steering.extend(theirs.steering);
                }
                Err(fail) => log::warn!("{host}'s status: {fail}"),
            }
        }
        Ok(status)
    }

    /// A setting every run follows, applied here and sent to every
    /// connected host (6.1), side by side. A configuration goes without its
    /// keys, which the host has from the starts (D11), and under the host's
    /// policy.
    pub async fn everywhere(&self, request: Request) -> Result<Response, Fail> {
        let reply = rovik_core::dispatch(&self.core, request.clone()).await?;
        let mut network = None;
        if matches!(request, Request::AgentConfigure(_)) {
            self.lock().configured = true;
            // As the settings file has them: another window's change of a
            // host's policy isn't sent back as it was.
            self.refresh_policies();
            network = host_network().await;
        }
        let sent = self.hosts.ready().into_iter().map(|(host, client)| {
            let mut request = request.clone();
            if let Request::AgentConfigure(config) = &mut request {
                **config = self.config_for(&host, config, &network);
            }
            async move {
                if let Err(fail) = client.call_within(request, HOSTS_WAIT).await {
                    log::warn!("{host} didn't take a setting: {fail}");
                }
            }
        });
        futures_util::future::join_all(sent).await;
        Ok(reply)
    }

    /// Settings → Network changed how cloud hosts connect (their rows, the
    /// timeout, a proxy or its password they go through): each ready host
    /// is sent the configuration again, with it. Before the frontend
    /// applied its settings nothing goes, as on connecting.
    pub async fn send_host_network(&self) {
        if !self.lock().configured {
            return;
        }
        self.refresh_policies();
        let network = host_network().await;
        let sent = self.hosts.ready().into_iter().map(|(host, client)| {
            let config = self.config_for(&host, &self.core.agents.config(), &network);
            async move {
                let request = Request::AgentConfigure(Box::new(config));
                if let Err(fail) = client.call_within(request, HOSTS_WAIT).await {
                    log::warn!("{host} didn't take how to connect: {fail}");
                }
            }
        });
        futures_util::future::join_all(sent).await;
    }

    /// On connecting (§2.3: "on connect and on every change"): the
    /// configuration as `everywhere` last applied it, to a host that wasn't
    /// ready then. A host set up during the session otherwise ran this
    /// computer's chats with its daemon's defaults (no approvals, no
    /// subagents) until the app's next start or the next change of a
    /// setting, and one back from being away kept the settings from before
    /// it went. Before the frontend applied its settings nothing goes: the
    /// host keeps what it has rather than take a configuration nobody set,
    /// and the frontend's reaches it, ready by then.
    ///
    /// What the host keeps for runs that go on without this computer
    /// (`AutonomyConfigure`, H02) goes whether or not the frontend applied
    /// its settings: it's the host's own settings', on this computer.
    async fn configure(&self, host: &str) {
        let Ok(client) = self.client(host) else {
            return;
        };
        self.refresh_policies();
        if self.lock().configured {
            let config = self.config_for(host, &self.core.agents.config(), &host_network().await);
            let sent = client.call_within(Request::AgentConfigure(Box::new(config)), HOSTS_WAIT);
            if let Err(fail) = sent.await {
                log::warn!("{host} didn't take the configuration: {fail}");
            }
        }
        if self.serves(host, features::AUTONOMY) {
            let autonomy = Request::AutonomyConfigure(self.autonomy_config(host));
            if let Err(fail) = client.call_within(autonomy, HOSTS_WAIT).await {
                log::warn!("{host} didn't take what it keeps for its own work: {fail}");
            }
        }
    }

    /// Before the project and its chats are deleted (K38): its chats on the
    /// home host (WSL, `State::home`), which are this computer's, are
    /// deleted there as each chat's delete would; one on a cloud host, or
    /// moving, refuses it.
    pub async fn before_deleting_project(&self, project_id: &str) -> Result<(), String> {
        for chat in self.home_chats(project_id, "deleting it").await? {
            self.delete(&chat).await.map_err(|fail| fail.message)?;
        }
        Ok(())
    }

    /// Before the project is merged into another (K38): its chats on the
    /// home host come back to this computer first, their results as their
    /// isolated changes, as the daemon keeps them under this project; their
    /// next start takes them there again, under the project they're merged
    /// into. One running there, one that can't come back (its folder
    /// missing, conflicts waiting), one on a cloud host, or moving, refuses
    /// it.
    pub async fn before_merging_project(&self, project_id: &str) -> Result<(), String> {
        let doing = "merging it";
        let chats = self.home_chats(project_id, doing).await?;
        let running = {
            let state = self.lock();
            chats.iter().any(|chat| state.running.contains_key(chat))
        };
        if running {
            return Err(format!(
                "A chat of this project is running in {}: stop it, or let it finish, before {doing}.",
                self.home_name()
            ));
        }
        for chat in chats {
            if let Err(fail) = self.move_back(&chat, None).await {
                return Err(format!(
                    "A chat of this project runs in {} and has to come back to this computer \
                     before {doing}, but it can't: {}",
                    self.home_name(),
                    fail.message
                ));
            }
        }
        Ok(())
    }

    /// The project's chats on the home host, or why the project can't
    /// change now: it has a chat on a cloud host, which only the user moves
    /// back, or one moving, to a host or back. `doing` ends the sentence:
    /// "deleting it", "merging it".
    async fn home_chats(&self, project_id: &str, doing: &str) -> Result<Vec<String>, String> {
        let id = project_id.to_string();
        let away = self
            .core
            .db
            .read(move |c| location::project_chats_away(c, &id))
            .await?;
        let home = self.home();
        let mut chats = Vec::new();
        for (chat, at) in away {
            match at {
                Location::Host(host) if Some(&host) == home.as_ref() => {
                    if self.is_moving_back(&chat) {
                        return Err(format!(
                            "A chat of this project is moving back from {}: wait for the move to \
                             end before {doing}.",
                            self.host_name(&host)
                        ));
                    }
                    chats.push(chat);
                }
                Location::Host(host) => {
                    return Err(format!(
                        "This project has chats on {}. Move them back to this computer before \
                         {doing}.",
                        self.host_name(&host)
                    ))
                }
                Location::Moving(host) => {
                    return Err(format!(
                        "A chat of this project is moving to {}: wait for the move to end before \
                         {doing}.",
                        self.host_name(&host)
                    ))
                }
                Location::Here | Location::Released => {}
            }
        }
        Ok(chats)
    }

    /// What the user calls the home host: "WSL".
    fn home_name(&self) -> String {
        self.home()
            .map_or_else(|| "WSL".to_string(), |home| self.host_name(&home))
    }

    /*
     * Events
     */

    async fn receive(&self, host: &str, incoming: Incoming) {
        match incoming {
            Incoming::State(state) => {
                log::info!("{host}: {state:?}");
                // Away, it has to catch up again once it's back.
                if state != HostState::Ready {
                    self.lock().caught_up.remove(host);
                }
                if let Some(cloud) = self.cloud() {
                    let problem = match state {
                        HostState::Refused => self.hosts.refusal(host),
                        _ => None,
                    };
                    let host = host.to_string();
                    cloud(CloudEvent::Host {
                        host,
                        state,
                        problem,
                    });
                }
            }
            Incoming::Subscribed { complete } => {
                self.configure(host).await;
                if !complete {
                    // A `PageClosed` may be among the events missed.
                    self.stop_forwarding(host);
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
                Event::PageWanted {
                    dialog_id,
                    id,
                    url,
                    port,
                } => self.page_wanted(host, dialog_id, id, url, port),
                Event::PageClosed { dialog_id, port } => self.page_closed(host, &dialog_id, port),
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
        // The daemon sends only this computer's chats: a second guard. A
        // chat not placed here yet, a subagent's made on the host while
        // this computer was away and brought by a pull, is placed by what
        // the database says of it.
        if let Some(dialog) = event.dialog_id() {
            if self.host_of(dialog).as_deref() != Some(host)
                && !self.placed_by_location(dialog, host).await
            {
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
            AgentEvent::Notice { title, .. } => {
                *title = format!("{}: {title}", self.host_name(host));
            }
            AgentEvent::ApprovalRequested { request } => {
                *request = mark_remote(request.clone());
            }
            AgentEvent::ContextUpdated { status, .. } => status.real_path = None,
            AgentEvent::RunStarted {
                dialog_id, run_id, ..
            } => {
                self.lock()
                    .running
                    .insert(dialog_id.clone(), run_id.clone());
            }
            AgentEvent::RunEnded { dialog_id, .. } => {
                self.lock().running.remove(dialog_id);
                self.pull_later(host, dialog_id, Duration::ZERO);
            }
            AgentEvent::TaskDriving { dialog_id, driving } => {
                let mut state = self.lock();
                match driving {
                    true => state.driving.insert(dialog_id.clone()),
                    false => state.driving.remove(dialog_id),
                };
            }
            _ => {}
        }
        // Another window's project: its approvals, questions and notices
        // show there, once.
        if let Some(dialog) = event.dialog_id() {
            if self.handled_elsewhere(dialog).await {
                return;
            }
        }
        self.core.emit(event);
    }

    /// Whether the database puts the chat on `host`: placed here when it
    /// does, so its events and requests are routed from then on.
    async fn placed_by_location(&self, dialog_id: &str, host: &str) -> bool {
        let id = dialog_id.to_string();
        let located = self.core.db.read(move |c| location::of_chat(c, &id)).await;
        match located {
            Ok(Location::Host(at)) if at == host => {
                self.place(dialog_id, host);
                true
            }
            _ => false,
        }
    }

    /// Subagents' chats that this computer learns of from the host's rows,
    /// not from a `SubagentStarted` it saw (made while this computer was
    /// away): placed here, and the frontend told of each as of one that
    /// started, so their chats show as subagents' and their approvals can be
    /// answered.
    pub(super) async fn note_children(&self, host: &str, chats: &[String]) {
        for chat in chats {
            let id = chat.clone();
            let child = self
                .core
                .db
                .read(move |c| store::child_dialog(c, &id))
                .await;
            let Ok(Some((child, title))) = child else {
                continue;
            };
            if self.host_of(&child.parent_id).as_deref() != Some(host) {
                continue;
            }
            self.place(&child.id, host);
            self.core.emit(AgentEvent::SubagentStarted {
                dialog_id: child.parent_id,
                tool_call_id: child.parent_call_id,
                child_dialog_id: child.id,
                kind: child.kind,
                model: String::new(),
                goal: title,
            });
        }
    }

    /// Back after missing events (4.3): the frontend is told what runs and
    /// waits on the host now, of the runs that ended meanwhile, and of the
    /// subagents going there, whose chats it may not have seen start.
    async fn catch_up(&self, host: &str) {
        let Some(mut status) = self.status_of(host).await else {
            return;
        };
        for subagent in std::mem::take(&mut status.subagents) {
            if self.host_of(&subagent.parent_dialog_id).as_deref() != Some(host) {
                continue;
            }
            self.place(&subagent.dialog_id, host);
            if let Some(info) = subagent.info {
                let started = AgentEvent::SubagentStarted {
                    dialog_id: subagent.parent_dialog_id,
                    tool_call_id: subagent.parent_call_id,
                    child_dialog_id: subagent.dialog_id,
                    kind: info.kind,
                    model: info.model,
                    goal: info.goal,
                };
                self.agent_event(host, started).await;
            }
        }
        // Its drivers at work now are told of first, and those done since
        // last: between the two, its tasks' stages never show as cut short.
        let driving: HashSet<String> = std::mem::take(&mut status.driving).into_iter().collect();
        for dialog_id in &driving {
            let event = AgentEvent::TaskDriving {
                dialog_id: dialog_id.clone(),
                driving: true,
            };
            self.agent_event(host, event).await;
        }
        let done: Vec<String> = self
            .lock()
            .driving
            .iter()
            .filter(|chat| !driving.contains(*chat))
            .cloned()
            .collect();
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
                    autopilot: false,
                    stage: None,
                };
                self.agent_event(host, event).await;
            }
        }
        for run in runs {
            let event = AgentEvent::RunStarted {
                dialog_id: run.dialog_id,
                run_id: run.run_id,
                steerable: run.steerable,
            };
            self.agent_event(host, event).await;
        }
        for dialog_id in done {
            if self.host_of(&dialog_id).as_deref() == Some(host) {
                let event = AgentEvent::TaskDriving {
                    dialog_id,
                    driving: false,
                };
                self.agent_event(host, event).await;
            }
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
    client
        .daemon()
        .is_some_and(|d| d.serves(features::PROJECTS))
}

/// The editor context names files from the project's root, not by a local
/// path that means nothing on the server (K08), and with `/` between their
/// folders, as the server's do (R04).
fn relative_paths(request: &mut StartRequest, root: &str) {
    let relative = |text: &str| relative_to(text, root);
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

/// `text` with each path under `root` written from it: `root` and the
/// separator after it go, and the path's own separators are `/` up to the
/// space or bracket that ends it. `/` and `\` are one in `root`, as a
/// Windows path may have either.
fn relative_to(text: &str, root: &str) -> String {
    let slashed = |s: &str| s.replace('\\', "/");
    let root = slashed(root.trim_end_matches(['/', '\\']));
    if root.is_empty() {
        return text.to_string();
    }
    // Both separators are a byte: the offsets in one are the other's.
    let haystack = slashed(text);
    let mut out = String::with_capacity(text.len());
    let mut from = 0;
    while let Some(found) = haystack[from..].find(&root) {
        let (at, after) = (from + found, from + found + root.len());
        if !haystack[after..].starts_with('/') {
            out.push_str(&text[from..after]);
            from = after;
            continue;
        }
        out.push_str(&text[from..at]);
        let path = after + 1;
        let end = haystack[path..]
            .find(|c: char| c.is_whitespace() || "]\"'<>()`,;".contains(c))
            .map_or(haystack.len(), |n| path + n);
        out.push_str(&haystack[path..end]);
        from = end;
    }
    out.push_str(&text[from..]);
    out
}

/// Whose files go up (`Remote::files_up`): a send's, which starts a run,
/// or a steer's, for a run that goes.
#[derive(Clone, Copy)]
enum Upload {
    Send,
    Steer,
}

/// An edit on the server is shown apart from the local file (K09).
fn mark_remote(mut request: ApprovalRequest) -> ApprovalRequest {
    request.remote = request.edit.is_some();
    request
}

fn internal(message: String) -> Fail {
    Fail::new(FailCode::Internal, message)
}

/// How the hosts are to connect (Settings → Network), the proxies'
/// passwords with it; none when it says nothing but "the host's own way".
async fn host_network() -> Option<HostNetwork> {
    Some(rovik_core::net::host_network().await).filter(|n| *n != HostNetwork::default())
}

#[cfg(test)]
mod tests;

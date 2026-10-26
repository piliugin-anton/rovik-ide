//! The agent, run in the backend.
//!
//! A run is a user message (or a Continue, a Retry, a resumed chat) and
//! every model turn and tool call after it. It runs here, as a task of its
//! own, independent of the webview: switching the sidebar away from the
//! chat, reloading the page or opening another chat doesn't touch it. The
//! frontend starts and stops runs, answers the approvals and questions a run
//! waits on, and follows along through `agent-event` events (events.rs);
//! everything a run produces is stored in the chat database as it happens.
//!
//!   run.rs        the loop: model turns, the end-of-run checks, pausing
//!   execute.rs    one turn's tool calls: permission, approval, running them
//!   review.rs     the AI reviewer (approval "by AI")
//!   verify.rs     the verification gate: the project's checks after a run
//!   before.rs     a failed check against the code before the change
//!   repair.rs     its fix rounds: progress, rollback, test integrity
//!   guard.rs      checkpoints and loop detection
//!   subagent.rs, subagent_run.rs: helper runs the lead starts (`task`)
//!   change_review.rs  a fresh-context review of the change before the checks
//!   guide.rs      after the second review that found problems: where and
//!                 why the agent goes wrong, with the review's findings
//!   tools.rs      tool schemas and running a tool
//!   steer.rs      messages the user sends a run while it works
//!   autopilot/    a task carried out with nobody there: its stages
//!   candidates.rs Thorough's two implementers, each in its own copy, and
//!                 the pick of the better
//!   permissions.rs, plan.rs, project.rs, repetition.rs, diff.rs,
//!   command_output.rs: the rules and text they use

pub mod acceptance;
pub mod autopilot;
mod before;
pub mod breaker;
pub mod budget;
pub mod candidates;
pub mod causes;
pub mod change_review;
pub mod check_output;
pub mod command_output;
pub mod diff;
#[cfg(test)]
mod e2e_tests;
pub mod environment;
pub mod events;
mod execute;
pub mod failure;
pub mod group;
pub mod guard;
pub mod guide;
pub mod judge;
mod landing;
pub mod log;
pub mod loops;
mod parallel;
pub mod paths;
pub mod permissions;
pub mod plan;
pub mod plan_review;
pub mod planning;
#[cfg(target_os = "linux")]
mod ports;
pub mod prepare;
pub mod project;
pub mod recovery;
pub mod repair;
mod repair_run;
pub mod repetition;
pub mod report;
pub mod restricted;
pub mod review;
mod run;
pub mod server;
pub mod steer;
pub mod subagent;
mod subagent_run;
pub mod test_writer;
pub mod tools;
pub mod triage;
mod unreached;
mod verify;
pub mod worker;

use crate::agent::OpenAIChatOptions;
use crate::chat_db::store::{self, NewMessage};
use crate::core::Core;
use events::{AgentEvent, ApprovalRequest, QuestionsRequest};
use plan::QuestionsOutcome;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{oneshot, watch};

pub use landing::{checks_would_run, pending_reason, pending_reason_in};
pub use rovik_protocol::agent::{
    AgentConfig, AgentStatus, ApprovalDecision, ChildInfo, ModelProfile, RunningRun,
    RunningSubagent, StartKind, StartRequest, SteerReply, UserMessage,
};
pub use run::last_user_wire_content;

/// Stop: set once, seen by the run between steps and by its current request.
pub struct StopSignal {
    flag: AtomicBool,
    tx: watch::Sender<bool>,
}

impl StopSignal {
    pub(crate) fn new() -> Self {
        StopSignal {
            flag: AtomicBool::new(false),
            tx: watch::channel(false).0,
        }
    }

    pub fn is_set(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    pub(crate) fn set(&self) {
        self.flag.store(true, Ordering::SeqCst);
        // Kept with no request listening too (`send` would drop it), so a
        // request that subscribes later still sees it.
        self.tx.send_replace(true);
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<bool> {
        self.tx.subscribe()
    }
}

/// A pause asked of a run (6.8, R02): it stops at its next safe point,
/// where a checkpoint pause happens, with this kind ("move" or "drain")
/// and, for a move, the host the chat goes to.
#[derive(Clone, Debug, PartialEq)]
pub struct PauseRequest {
    pub kind: &'static str,
    pub host: Option<String>,
}

/// A pause, once asked: seen by the run at its safe points.
#[derive(Default)]
pub struct PauseSignal(Mutex<Option<PauseRequest>>);

impl PauseSignal {
    pub fn get(&self) -> Option<PauseRequest> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Asks for the pause. A move's pause stays through a drain: the run
    /// ends at the same point either way, and the move, which waits for
    /// a pause of its kind, goes on where the chat goes.
    fn set(&self, request: PauseRequest) {
        let mut asked = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if asked.as_ref().is_some_and(|a| a.kind == MOVE) && request.kind != MOVE {
            return;
        }
        *asked = Some(request);
    }

    /// Takes back a move's pause (the move was cancelled before the run
    /// saw it). A drain's stays.
    fn clear_move(&self) {
        let mut asked = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if asked.as_ref().is_some_and(|a| a.kind == MOVE) {
            *asked = None;
        }
    }
}

/// The pause kinds a run can be asked for (`PauseInfo::kind`).
pub const MOVE: &str = "move";

struct RunControl {
    run_id: String,
    /// A subagent's run: its lead's chat and `task` call. Stop there stops it too.
    parent: Option<String>,
    parent_call: Option<String>,
    /// The run's model (`ModelProfile::id`): an effort chosen for it applies.
    profile_id: String,
    stop: Arc<StopSignal>,
    /// A lead's: a move or a drain asked of it. Its subagents' runs have
    /// their own, never set, so they finish first.
    pause: Arc<PauseSignal>,
    /// The reasoning effort picked while the run goes on (see `set_effort`).
    effort: watch::Sender<EffortPick>,
    /// The messages the user sends it while it works (steer.rs). A
    /// subagent's run has one that never takes any.
    steer: Arc<steer::SteerQueue>,
    /// A subagent's: what its block shows, for a page reloaded mid-run (P1.5).
    child: Option<ChildInfo>,
    /// A lead's that carries out an Autopilot task's stage, with the task's
    /// effort: it and its subagents follow the configuration with the
    /// kinds the pipeline needs on (D17, `autopilot::subagent_config`).
    autopilot: Option<autopilot::Effort>,
    /// An Autopilot lead's (U04): when it started, what its replies and its
    /// subagents' cost so far, and what the task spent before it.
    started: std::time::Instant,
    spent: budget::Spent,
    spent_before: budget::Spent,
    /// An Autopilot lead's (U04, R03): one of its subagents found the task
    /// past its budget and stopped the run, which ends `budget`: why.
    budget_cut: Option<String>,
    /// The chat held for this process while the run goes (chat_hold.rs).
    _hold: crate::chat_hold::Hold,
    /// A lead's run's task, once spawned (`start_run`): a move cuts a run
    /// that only waits for the user (`cut_waiting`).
    abort: Option<tokio::task::AbortHandle>,
}

/// What `AgentState::cut_waiting` took from a run it cut: the approvals and
/// questions it waited on. Kept until the run has ended, then dropped: a
/// reply dropped before would reach a run still being polled as a "no".
pub struct CutWaits {
    _approvals: Vec<Waiting<ApprovalDecision, ApprovalRequest>>,
    _questions: Vec<Waiting<QuestionsOutcome, QuestionsRequest>>,
}

/// A reasoning effort picked while a run goes on.
#[derive(Clone, Debug, Default)]
pub struct EffortPick {
    pub options: Option<OpenAIChatOptions>,
    /// Picked in this run's chat: a request that stopped answering is asked
    /// again with it. Picked elsewhere (the effort is per model), it waits
    /// for the next request.
    pub here: bool,
}

/// What a run is handed when it's registered.
pub(super) struct Controls {
    pub run_id: String,
    pub stop: Arc<StopSignal>,
    pub pause: Arc<PauseSignal>,
    pub effort: watch::Receiver<EffortPick>,
    pub steer: Arc<steer::SteerQueue>,
}

struct Waiting<T, R> {
    request: R,
    reply: oneshot::Sender<T>,
}

/// (dialog id, tool call id)
type WaitKey = (String, String);

/// The configuration a chat's runs follow, where it isn't the one
/// configuration: a daemon keeps one for each computer (D16). None: the one.
pub type ConfigResolver = Arc<dyn Fn(&str) -> Option<AgentConfig> + Send + Sync>;

/// The files a chat was given conflict markers in that still have them,
/// from its run's root, where it isn't an overlay that knows: a daemon
/// knows its worktrees' (6.7).
pub type MarkersResolver = Arc<dyn Fn(&str) -> Vec<String> + Send + Sync>;

#[derive(Default)]
struct Inner {
    config: AgentConfig,
    resolver: Option<ConfigResolver>,
    markers: Option<MarkersResolver>,
    /// The runs work in a server's worktrees (D8, server.rs).
    on_server: bool,
    /// What the server does with a task's result it delivers (D15, H04).
    deliverer: Option<Arc<dyn autopilot::Deliverer>>,
    runs: HashMap<String, RunControl>,
    /// Keyed by chat and tool call: providers number calls per reply
    /// (Gemini: "function-call-1"), so two chats can wait on the same id.
    approvals: HashMap<WaitKey, Waiting<ApprovalDecision, ApprovalRequest>>,
    questions: HashMap<WaitKey, Waiting<QuestionsOutcome, QuestionsRequest>>,
    /// A run's fix-round state while its checks wait for the user's
    /// approval, by chat (verify.rs `VerifyCarry`).
    carries: HashMap<String, verify::VerifyCarry>,
    /// The daemon is stopping (R02): its runs pause, and no new one starts.
    draining: bool,
    /// How long an Autopilot task's approval waits for an answer (U03);
    /// None: `APPROVAL_DEADLINE`. Tests make it short.
    approval_deadline: Option<std::time::Duration>,
    /// T05: a Thorough task's candidates' chats, by chat: the chat whose
    /// task each implements, whose configuration it follows, and its Stop.
    candidates: HashMap<String, Candidacy>,
    /// T05: where a server's candidates implement (its sibling worktrees).
    copies: Option<Arc<dyn candidates::Copies>>,
    /// The chats whose Autopilot driver is at work, by how many of its
    /// turns are (autopilot::AtWork).
    driving: HashMap<String, usize>,
    /// Chats whose files change outside any run, as a server's merge of a
    /// newer snapshot changes them (6.7): no run of one starts until the
    /// hold goes, and a start waits for that. Its sender goes with it.
    files_held: HashMap<String, watch::Sender<()>>,
}

/// T05: what a candidate's chat implements for.
#[derive(Clone, Debug)]
struct Candidacy {
    /// The chat whose task it implements.
    main: String,
    /// That chat's project folder, whose settings its runs follow (its own
    /// folder is a copy).
    project: Option<String>,
    /// The call id its events and transcript go by in that chat.
    call: String,
    /// What its line in that chat shows, as a subagent's block does (P1.5).
    info: ChildInfo,
}

/// How long an approval in an Autopilot task waits before it's denied
/// (U03): one that reaches a person at all (U02 sends them all to the AI
/// reviewer) would otherwise wait for someone who isn't there.
pub const APPROVAL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Why a run doesn't start while the daemon stops (R02).
pub const RESTARTING: &str = "Rovik on the server is restarting: send it again in a minute.";

/// See `AgentState::registered`.
pub(super) struct Registered {
    state: AgentState,
    dialog_id: String,
    run_id: String,
}

impl Drop for Registered {
    fn drop(&mut self) {
        self.state.unregister(&self.dialog_id, &self.run_id);
    }
}

/// A chat's files held for a change outside any run (`AgentState::hold_files`),
/// let go when dropped.
pub struct FilesHold {
    state: AgentState,
    dialog_id: String,
}

impl Drop for FilesHold {
    fn drop(&mut self) {
        self.state.lock().files_held.remove(&self.dialog_id);
    }
}

/// Why a chat's files couldn't be held (`AgentState::hold_files`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotHeld {
    /// One of its runs goes, a subagent's of it, or its task's candidates'.
    Running,
    /// Another change outside its runs has them (a merge, say).
    Held,
}

/// Whether the chat's run goes, or a subagent's of it, or one of its task's
/// candidates' (T05).
fn running_in(inner: &Inner, dialog_id: &str) -> bool {
    let candidate = |chat: &str| {
        inner
            .candidates
            .get(chat)
            .is_some_and(|c| c.main == dialog_id)
    };
    inner.runs.contains_key(dialog_id)
        || inner.runs.iter().any(|(chat, r)| {
            r.parent.as_deref() == Some(dialog_id)
                // T05: its task's candidates implement for it.
                || candidate(chat)
                || r.parent.as_deref().is_some_and(candidate)
        })
}

/// Runs in progress (one per chat at most), what they wait on, and the settings.
#[derive(Clone, Default)]
pub struct AgentState(Arc<Mutex<Inner>>);

impl AgentState {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn config(&self) -> AgentConfig {
        self.lock().config.clone()
    }

    /// The configuration the chat's runs follow: the resolver's for it,
    /// where one is set, else the one configuration. A subagent's chat
    /// follows its lead's; an Autopilot task's run has the kinds its
    /// pipeline needs on (D17).
    pub fn config_for(&self, dialog_id: &str) -> AgentConfig {
        let (resolver, lead, effort) = {
            let inner = self.lock();
            let lead = Self::lead_of(&inner, dialog_id);
            let effort = inner.runs.get(lead).and_then(|r| r.autopilot);
            // A candidate's chat follows the configuration of the chat whose
            // task it implements (T05).
            let owner = inner.candidates.get(lead).map_or(lead, |c| c.main.as_str());
            (inner.resolver.clone(), owner.to_string(), effort)
        };
        // Asked without the lock: the resolver may take locks of its own.
        let config = resolver
            .and_then(|resolve| resolve(&lead))
            .unwrap_or_else(|| self.config());
        match effort {
            Some(effort) => autopilot::run_config(config, effort),
            None => config,
        }
    }

    /// The chat whose run a chat's run is under: its lead's, for a
    /// subagent's (and its own, for a lead's).
    fn lead_of<'a>(inner: &'a Inner, dialog_id: &'a str) -> &'a str {
        let mut chat = dialog_id;
        while let Some(parent) = inner.runs.get(chat).and_then(|r| r.parent.as_deref()) {
            chat = parent;
        }
        chat
    }

    /// The chat's run carries out a stage of an Autopilot task of this
    /// effort, which spent `before` until it: from now on it and its
    /// subagents follow `config_for`'s Autopilot configuration, and what
    /// they spend counts against the task's budget.
    pub(super) fn set_autopilot(
        &self,
        dialog_id: &str,
        effort: autopilot::Effort,
        before: budget::Spent,
    ) {
        if let Some(run) = self.lock().runs.get_mut(dialog_id) {
            run.autopilot = Some(effort);
            run.spent_before = before;
        }
    }

    /// A reply in the chat's run, or a subagent's of it: what it cost, for
    /// its Autopilot task's budget (U04). Nothing for any other run.
    pub(super) fn note_reply(&self, dialog_id: &str, usage: Option<&serde_json::Value>) {
        let mut inner = self.lock();
        let lead = Self::lead_of(&inner, dialog_id).to_string();
        if let Some(run) = inner.runs.get_mut(&lead).filter(|r| r.autopilot.is_some()) {
            run.spent.note_reply(usage);
        }
    }

    /// What the chat's Autopilot run (its lead's) spent itself so far: its
    /// time and its replies'; None: it's no task's.
    pub(super) fn run_spent(&self, dialog_id: &str) -> Option<budget::Spent> {
        let inner = self.lock();
        let run = inner.runs.get(Self::lead_of(&inner, dialog_id))?;
        run.autopilot?;
        Some(budget::Spent {
            ms: run.started.elapsed().as_millis() as u64,
            ..run.spent
        })
    }

    /// Why the chat's Autopilot task is past its budget, if it is: what it
    /// spent before this run and what this run spent so far, against the
    /// settings' limits.
    pub(super) fn over_budget(&self, dialog_id: &str) -> Option<String> {
        let spent = self.run_spent(dialog_id)?;
        let before = {
            let inner = self.lock();
            inner
                .runs
                .get(Self::lead_of(&inner, dialog_id))?
                .spent_before
        };
        let budget = self.config_for(dialog_id).autopilot.budget;
        budget::over(&budget, &before.plus(spent))
    }

    /// U04 (R03): a subagent of the chat's Autopilot run found its task past
    /// its budget at one of its replies: the lead's run stops, with its
    /// subagents, and ends `budget` (`budget_cut_of`), as at one of its own.
    pub(super) fn cut_for_budget(&self, core: &Core, dialog_id: &str, why: &str) {
        let lead = {
            let mut inner = self.lock();
            let lead = Self::lead_of(&inner, dialog_id).to_string();
            if let Some(run) = inner.runs.get_mut(&lead) {
                run.budget_cut.get_or_insert_with(|| why.to_string());
            }
            lead
        };
        self.stop(core, &lead);
    }

    /// Why the chat's run was cut for its task's budget, if it was
    /// (`cut_for_budget`).
    pub(super) fn budget_cut_of(&self, dialog_id: &str) -> Option<String> {
        self.lock().runs.get(dialog_id)?.budget_cut.clone()
    }

    /// The effort of the Autopilot task the chat's run, or its lead's,
    /// carries out a stage of; None: it's no task's.
    pub(super) fn autopilot_of(&self, dialog_id: &str) -> Option<autopilot::Effort> {
        let inner = self.lock();
        let lead = Self::lead_of(&inner, dialog_id);
        inner.runs.get(lead).and_then(|r| r.autopilot)
    }

    /// Runs follow the resolver's configuration from now on (a daemon's).
    pub fn set_resolver(&self, resolver: ConfigResolver) {
        self.lock().resolver = Some(resolver);
    }

    /// Where a run finds the files it still has conflict markers in, for
    /// its reminder when it ends (6.7): a daemon's worktrees.
    pub fn set_markers_resolver(&self, resolver: MarkersResolver) {
        self.lock().markers = Some(resolver);
    }

    fn markers_resolver(&self) -> Option<MarkersResolver> {
        self.lock().markers.clone()
    }

    /// The runs work in a server's worktrees from now on, under its rules
    /// (D8, server.rs): a daemon's.
    pub fn set_on_server(&self) {
        self.lock().on_server = true;
    }

    pub fn on_server(&self) -> bool {
        self.lock().on_server
    }

    /// A delivered task's result goes through `deliverer` from now on (D15,
    /// H04): a daemon's, which pushes it when its computer asked.
    pub fn set_deliverer(&self, deliverer: Arc<dyn autopilot::Deliverer>) {
        self.lock().deliverer = Some(deliverer);
    }

    pub fn deliverer(&self) -> Option<Arc<dyn autopilot::Deliverer>> {
        self.lock().deliverer.clone()
    }

    /// Keeps a run's fix-round state until the user starts its checks
    /// (one per chat: a newer one replaces it). Carries past their time go.
    fn put_carry(&self, dialog_id: &str, carry: verify::VerifyCarry) {
        let mut inner = self.lock();
        inner
            .carries
            .retain(|_, c| c.stored.elapsed() < verify::CARRY_TTL);
        inner.carries.insert(dialog_id.to_string(), carry);
    }

    /// The state a run left for the checks waiting in `record_id`, if it
    /// left it for those. Another record's (an older one) leaves the chat's
    /// carry as it is (P1.7).
    fn take_carry(&self, dialog_id: &str, record_id: &str) -> Option<verify::VerifyCarry> {
        let mut inner = self.lock();
        let c = inner.carries.get(dialog_id)?;
        if c.record_id != record_id {
            return None;
        }
        let carry = inner.carries.remove(dialog_id)?;
        (carry.stored.elapsed() < verify::CARRY_TTL).then_some(carry)
    }

    /// Drops a chat's carry: deleted or discarded, or a newer run ended
    /// without leaving checks waiting (P1.8).
    pub fn drop_carry(&self, dialog_id: &str) {
        self.lock().carries.remove(dialog_id);
    }

    /// Makes the chat's carry `by` older (its time running out).
    #[cfg(test)]
    pub(super) fn age_carry(&self, dialog_id: &str, by: std::time::Duration) {
        if let Some(c) = self.lock().carries.get_mut(dialog_id) {
            c.stored -= by;
        }
    }

    #[cfg(test)]
    pub(super) fn has_carry(&self, dialog_id: &str) -> bool {
        self.lock().carries.contains_key(dialog_id)
    }

    fn set_config(&self, config: AgentConfig) {
        self.lock().config = config;
    }

    /// Registers a run for the chat; None when one is already going there,
    /// or a drain is under way (checked under the lock `drain` sets it in,
    /// so no lead's run starts after the drain has asked the others to pause).
    fn register(
        &self,
        dialog_id: &str,
        profile_id: &str,
        options: Option<OpenAIChatOptions>,
    ) -> Option<Controls> {
        self.register_in(dialog_id, None, profile_id, options)
    }

    /// Registers a subagent's run in its own chat, under its lead's chat.
    /// Stopped already when the lead was stopped meanwhile.
    pub(super) fn register_child(
        &self,
        dialog_id: &str,
        parent: &str,
        parent_call: &str,
        profile_id: &str,
        options: Option<OpenAIChatOptions>,
    ) -> Option<Controls> {
        let controls =
            self.register_in(dialog_id, Some((parent, parent_call)), profile_id, options)?;
        let parent_stopped = self.lock().runs.get(parent).is_none_or(|r| r.stop.is_set());
        if parent_stopped {
            controls.stop.set();
        }
        Some(controls)
    }

    fn register_in(
        &self,
        dialog_id: &str,
        parent: Option<(&str, &str)>,
        profile_id: &str,
        options: Option<OpenAIChatOptions>,
    ) -> Option<Controls> {
        // Held first: another process at work on the chat keeps it.
        let hold = crate::chat_hold::take(dialog_id)?;
        let mut inner = self.lock();
        if inner.runs.contains_key(dialog_id)
            || inner.files_held.contains_key(dialog_id)
            || (parent.is_none() && inner.draining)
        {
            return None;
        }
        let run_id = uuid::Uuid::new_v4().to_string();
        let stop = Arc::new(StopSignal::new());
        let pause = Arc::new(PauseSignal::default());
        let (effort_tx, effort_rx) = watch::channel(EffortPick {
            options,
            here: false,
        });
        let steer = Arc::new(steer::SteerQueue::default());
        inner.runs.insert(
            dialog_id.to_string(),
            RunControl {
                run_id: run_id.clone(),
                parent: parent.map(|(p, _)| p.to_string()),
                parent_call: parent.map(|(_, c)| c.to_string()),
                profile_id: profile_id.to_string(),
                stop: stop.clone(),
                pause: pause.clone(),
                effort: effort_tx,
                steer: steer.clone(),
                child: None,
                autopilot: None,
                started: std::time::Instant::now(),
                spent: budget::Spent::default(),
                spent_before: budget::Spent::default(),
                budget_cut: None,
                _hold: hold,
                abort: None,
            },
        );
        Some(Controls {
            run_id,
            stop,
            pause,
            effort: effort_rx,
            steer,
        })
    }

    /// Asks the chat's run to pause at its next safe point. False when no
    /// run of its own goes there (a subagent's chat pauses with its lead).
    pub fn pause(&self, dialog_id: &str, request: PauseRequest) -> bool {
        let inner = self.lock();
        match inner.runs.get(dialog_id).filter(|r| r.parent.is_none()) {
            Some(run) => {
                run.pause.set(request);
                true
            }
            None => false,
        }
    }

    /// The pause asked of the chat's run, if any.
    #[cfg(test)]
    pub(crate) fn pause_asked(&self, dialog_id: &str) -> Option<PauseRequest> {
        self.lock().runs.get(dialog_id)?.pause.get()
    }

    /// Asks every lead's run to pause for a drain, and lets no new one
    /// start: their chats.
    pub fn drain(&self) -> Vec<String> {
        let mut inner = self.lock();
        inner.draining = true;
        let request = PauseRequest {
            kind: recovery::DRAIN,
            host: None,
        };
        inner
            .runs
            .iter()
            .filter(|(_, r)| r.parent.is_none())
            .map(|(chat, run)| {
                run.pause.set(request.clone());
                chat.clone()
            })
            .collect()
    }

    /// A reasoning effort chosen for a model: the runs using it take it for
    /// their next request. The run in the chat it was picked in (`dialog_id`)
    /// also asks again at once if its request stopped answering before any of
    /// the reply arrived (run.rs). Returns how many runs it reached.
    pub fn set_effort(
        &self,
        profile_id: &str,
        options: Option<OpenAIChatOptions>,
        dialog_id: Option<&str>,
    ) -> usize {
        self.set_effort_where(profile_id, options, dialog_id, |_| true)
    }

    /// `set_effort`, for the runs of the chats `reaches` takes only: a
    /// server's, one computer's chats (D16).
    pub fn set_effort_where(
        &self,
        profile_id: &str,
        options: Option<OpenAIChatOptions>,
        dialog_id: Option<&str>,
        reaches: impl Fn(&str) -> bool,
    ) -> usize {
        let inner = self.lock();
        let mut reached = 0;
        for (dialog, run) in inner
            .runs
            .iter()
            .filter(|(chat, r)| r.profile_id == profile_id && r.parent.is_none() && reaches(chat))
        {
            let here = dialog_id == Some(dialog.as_str());
            run.effort.send_replace(EffortPick {
                options: options.clone(),
                here,
            });
            reached += 1;
        }
        reached
    }

    pub(super) fn unregister(&self, dialog_id: &str, run_id: &str) {
        let mut inner = self.lock();
        if inner
            .runs
            .get(dialog_id)
            .is_some_and(|r| r.run_id == run_id)
        {
            inner.runs.remove(dialog_id);
        }
    }

    /// A guard that unregisters the run when dropped, however its task
    /// ends: a panic or an aborted future too.
    pub(super) fn registered(&self, dialog_id: &str, run_id: &str) -> Registered {
        Registered {
            state: self.clone(),
            dialog_id: dialog_id.to_string(),
            run_id: run_id.to_string(),
        }
    }

    /// What a subagent's block shows (P1.5): kept with its run.
    pub(super) fn describe_child(&self, dialog_id: &str, info: ChildInfo) {
        if let Some(run) = self.lock().runs.get_mut(dialog_id) {
            run.child = Some(info);
        }
    }

    /// A subagent's progress, or a candidate's (T05), as its block's or
    /// its line's event says.
    pub(super) fn note_child_progress(&self, dialog_id: &str, tool_calls: u32, model: &str) {
        let mut inner = self.lock();
        let inner = &mut *inner;
        let info = match inner.runs.get_mut(dialog_id).and_then(|r| r.child.as_mut()) {
            Some(info) => Some(info),
            None => inner.candidates.get_mut(dialog_id).map(|c| &mut c.info),
        };
        if let Some(info) = info {
            info.tool_calls = tool_calls;
            info.model = model.to_string();
        }
    }

    /// How many subagents are running for the chat.
    pub fn running_children(&self, dialog_id: &str) -> usize {
        self.lock()
            .runs
            .values()
            .filter(|r| r.parent.as_deref() == Some(dialog_id))
            .count()
    }

    /// One more of the chat's Autopilot driver's turns at work (`more`),
    /// or one fewer: whether the chat's driving starts (true) or ends
    /// (false) with it; None while it goes on as it was.
    pub(super) fn turn_driving(&self, dialog_id: &str, more: bool) -> Option<bool> {
        let mut inner = self.lock();
        let turns = inner.driving.entry(dialog_id.to_string()).or_default();
        let was = *turns > 0;
        *turns = match more {
            true => *turns + 1,
            false => turns.saturating_sub(1),
        };
        let is = *turns > 0;
        if !is {
            inner.driving.remove(dialog_id);
        }
        (was != is).then_some(is)
    }

    /// Whether the chat's Autopilot driver is at work (`turn_driving`).
    pub fn is_driving(&self, dialog_id: &str) -> bool {
        self.lock().driving.contains_key(dialog_id)
    }

    /// Whether the chat has a run going (a lead's subagents count as its own).
    pub fn is_running(&self, dialog_id: &str) -> bool {
        running_in(&self.lock(), dialog_id)
    }

    /// Holds the chat's files for a change outside any run, as a server's
    /// merge of a newer snapshot into its worktree (6.7): refused while one
    /// of its runs goes (or its task's candidates), or another hold has
    /// them. Until the hold drops, a run of the chat waits to start,
    /// whatever starts it: a message, a restart, its task's next stage.
    pub fn hold_files(&self, dialog_id: &str) -> Result<FilesHold, NotHeld> {
        let mut inner = self.lock();
        if running_in(&inner, dialog_id) {
            return Err(NotHeld::Running);
        }
        if inner.files_held.contains_key(dialog_id) {
            return Err(NotHeld::Held);
        }
        let (held, _) = watch::channel(());
        inner.files_held.insert(dialog_id.to_string(), held);
        Ok(FilesHold {
            state: self.clone(),
            dialog_id: dialog_id.to_string(),
        })
    }

    /// Whether a hold has the chat's files (`hold_files`).
    fn files_held(&self, dialog_id: &str) -> bool {
        self.lock().files_held.contains_key(dialog_id)
    }

    /// Once no hold has the chat's files.
    async fn files_let_go(&self, dialog_id: &str) {
        loop {
            let held = self.lock().files_held.get(dialog_id).map(|h| h.subscribe());
            let Some(mut held) = held else {
                return;
            };
            // Ends once the hold's sender goes with it.
            while held.changed().await.is_ok() {}
        }
    }

    /// T05: `candidate` is a chat implementing `main`'s task, in a copy of
    /// `main`'s project folder `project`: it follows `main`'s configuration
    /// and its Stop, and the project's settings; `call` is the id its
    /// events go by in `main`, and `info` what its line there shows.
    pub(super) fn add_candidate(
        &self,
        candidate: &str,
        main: &str,
        project: Option<String>,
        call: &str,
        info: ChildInfo,
    ) {
        let candidacy = Candidacy {
            main: main.to_string(),
            project,
            call: call.to_string(),
            info,
        };
        self.lock()
            .candidates
            .insert(candidate.to_string(), candidacy);
    }

    /// T05: the chat is no candidate's any more (its task picked).
    pub(super) fn drop_candidate(&self, candidate: &str) {
        self.lock().candidates.remove(candidate);
    }

    /// T05: the chat whose task a candidate implements, and the call id its
    /// events go by there.
    pub(super) fn candidate_call(&self, candidate: &str) -> Option<(String, String)> {
        let inner = self.lock();
        let c = inner.candidates.get(candidate)?;
        Some((c.main.clone(), c.call.clone()))
    }

    /// T05: the project folder whose settings a candidate's runs follow.
    pub(super) fn candidate_project(&self, candidate: &str) -> Option<String> {
        let inner = self.lock();
        inner.candidates.get(candidate)?.project.clone()
    }

    /// T05: where a server's candidates implement, set by the daemon.
    pub fn set_copies(&self, copies: Arc<dyn candidates::Copies>) {
        self.lock().copies = Some(copies);
    }

    pub(super) fn copies(&self) -> Option<Arc<dyn candidates::Copies>> {
        self.lock().copies.clone()
    }

    /// Whether the chat's run goes on only by waiting for the user: each
    /// of its runs, its own and its subagents', waits on an approval or a
    /// question, or on subagents of its own. Cut now, it loses nothing
    /// that `Resume` doesn't ask for again (5.4).
    pub fn waits_on_user(&self, dialog_id: &str) -> bool {
        let inner = self.lock();
        let mut family = vec![dialog_id.to_string()];
        let mut i = 0;
        while i < family.len() {
            let children = inner
                .runs
                .iter()
                .filter(|(_, r)| r.parent.as_deref() == Some(family[i].as_str()))
                .map(|(d, _)| d.clone());
            family.extend(children.collect::<Vec<_>>());
            i += 1;
        }
        let running: Vec<&String> = family
            .iter()
            .filter(|d| inner.runs.contains_key(d.as_str()))
            .collect();
        !running.is_empty()
            && running.iter().all(|d| {
                let asks = |key: &WaitKey| &key.0 == *d;
                inner.approvals.keys().any(asks)
                    || inner.questions.keys().any(asks)
                    || inner.runs.values().any(|r| r.parent.as_ref() == Some(*d))
            })
    }

    fn draining(&self) -> bool {
        self.lock().draining
    }

    /// The chat's run's task, once spawned: what `cut_waiting` aborts.
    fn set_abort(&self, dialog_id: &str, run_id: &str, abort: tokio::task::AbortHandle) {
        if let Some(run) = self
            .lock()
            .runs
            .get_mut(dialog_id)
            .filter(|r| r.run_id == run_id)
        {
            run.abort = Some(abort);
        }
    }

    /// Cuts the chat's run where it only waits for the user to approve a
    /// call: a lead's of an ordinary chat (no Autopilot task, no subagents
    /// or candidates at work, no question asked, no message of the user's
    /// waiting for it), as a crash would cut it.
    /// Nothing of it runs meanwhile: a call that needs an approval runs
    /// alone, before it starts (execute.rs `runs_in_parallel`), and the
    /// approval is the only wait the run registers. The calls are left
    /// without results, which a `Resume` asks about again (5.4) where the
    /// chat moves. A move's pause would otherwise wait, without end, for a
    /// decision about a call the move is about to take elsewhere. What it
    /// waited on is settled in the chat; the replies are kept until the run
    /// has ended (`CutWaits`). None when it isn't such a run.
    pub fn cut_waiting(&self, core: &Core, dialog_id: &str) -> Option<CutWaits> {
        let (approvals, questions) = {
            let mut inner = self.lock();
            let run = inner.runs.get(dialog_id)?;
            if run.parent.is_some() || run.autopilot.is_some() || run.abort.is_none() {
                return None;
            }
            if inner.driving.contains_key(dialog_id) {
                return None;
            }
            // Only the run itself: nothing of its family is at work.
            let others = inner.runs.iter().any(|(chat, r)| {
                chat != dialog_id
                    && (r.parent.as_deref() == Some(dialog_id)
                        || inner.candidates.get(chat).is_some_and(|c| c.main == dialog_id))
            });
            if others {
                return None;
            }
            // A message the user sent it meanwhile goes along with a pause,
            // stored first, and would only be handed back by a cut: the
            // move waits for the answer then, as it always did.
            if !run.steer.waiting().is_empty() {
                return None;
            }
            let own = |key: &WaitKey| key.0 == dialog_id;
            if !inner.approvals.keys().any(own) || inner.questions.keys().any(own) {
                return None;
            }
            if let Some(abort) = inner.runs.get(dialog_id).and_then(|r| r.abort.as_ref()) {
                abort.abort();
            }
            let keys: Vec<WaitKey> = inner.approvals.keys().filter(|k| own(k)).cloned().collect();
            let approvals: Vec<_> = keys
                .into_iter()
                .filter_map(|k| inner.approvals.remove(&k).map(|w| (k, w)))
                .collect();
            let keys: Vec<WaitKey> = inner.questions.keys().filter(|k| own(k)).cloned().collect();
            let questions: Vec<_> = keys
                .into_iter()
                .filter_map(|k| inner.questions.remove(&k).map(|w| (k, w)))
                .collect();
            (approvals, questions)
        };
        let mut cut = CutWaits {
            _approvals: Vec::new(),
            _questions: Vec::new(),
        };
        for ((dialog_id, tool_call_id), w) in approvals {
            core.emit(AgentEvent::ApprovalSettled {
                dialog_id,
                tool_call_id,
            });
            cut._approvals.push(w);
        }
        for ((dialog_id, tool_call_id), w) in questions {
            core.emit(AgentEvent::QuestionsSettled {
                dialog_id,
                tool_call_id,
            });
            cut._questions.push(w);
        }
        Some(cut)
    }

    /// Takes back a move's pause asked of the chat's run (the move was
    /// cancelled before the run saw it).
    pub fn unpause_move(&self, dialog_id: &str) {
        if let Some(run) = self.lock().runs.get(dialog_id) {
            run.pause.clear_move();
        }
    }

    /// Stops the chat's run: its current request, its next steps, and
    /// whatever it waits on (approvals count as denied, questions as stopped).
    /// A lead's subagents stop with it; a subagent's chat stops only itself.
    /// False when nothing ran there (neither the chat nor a subagent of it).
    pub fn stop(&self, core: &Core, dialog_id: &str) -> bool {
        self.stop_in(core, dialog_id, true)
    }

    /// Stops a lead's subagents only (the lead itself goes on, e.g. to
    /// report an error once they've ended).
    pub(super) fn stop_children(&self, core: &Core, dialog_id: &str) {
        self.stop_in(core, dialog_id, false);
    }

    fn stop_in(&self, core: &Core, dialog_id: &str, itself: bool) -> bool {
        // What waits is taken under the lock, and answered and told of
        // once it's let go: a sink that asks the state back would
        // otherwise wait on it.
        let (approvals, questions) = {
            let mut inner = self.lock();
            let own = if itself {
                inner
                    .runs
                    .get(dialog_id)
                    .map(|run| run.stop.set())
                    .is_some()
            } else {
                false
            };
            // T05: its task's candidates, and theirs, stop with it.
            let candidates = inner.candidates.clone();
            let candidate = |chat: &str| candidates.get(chat).is_some_and(|c| c.main == dialog_id);
            let children: Vec<String> = inner
                .runs
                .iter()
                .filter(|(chat, r)| {
                    r.parent.as_deref() == Some(dialog_id)
                        || (itself
                            && (candidate(chat) || r.parent.as_deref().is_some_and(candidate)))
                })
                .map(|(d, r)| {
                    r.stop.set();
                    d.clone()
                })
                .collect();
            // A lead that isn't registered (its run ended in an error) still
            // stops the subagents it left.
            if !own && children.is_empty() {
                return false;
            }
            let stopping = |d: &String| (itself && d == dialog_id) || children.contains(d);
            let approvals: Vec<WaitKey> = inner
                .approvals
                .keys()
                .filter(|(d, _)| stopping(d))
                .cloned()
                .collect();
            let approvals: Vec<_> = approvals
                .into_iter()
                .filter_map(|key| inner.approvals.remove(&key).map(|w| (key, w)))
                .collect();
            let questions: Vec<WaitKey> = inner
                .questions
                .keys()
                .filter(|(d, _)| stopping(d))
                .cloned()
                .collect();
            let questions: Vec<_> = questions
                .into_iter()
                .filter_map(|key| inner.questions.remove(&key).map(|w| (key, w)))
                .collect();
            (approvals, questions)
        };
        for ((dialog_id, tool_call_id), w) in approvals {
            let _ = w.reply.send(ApprovalDecision::Deny);
            core.emit(AgentEvent::ApprovalSettled {
                dialog_id,
                tool_call_id,
            });
        }
        for ((dialog_id, tool_call_id), w) in questions {
            let _ = w.reply.send(QuestionsOutcome::Stopped { stopped: true });
            core.emit(AgentEvent::QuestionsSettled {
                dialog_id,
                tool_call_id,
            });
        }
        true
    }

    /// Asks the user and waits for the decision (Deny if the run is stopped).
    pub async fn wait_for_approval(
        &self,
        core: &Core,
        request: ApprovalRequest,
        stop: &StopSignal,
    ) -> ApprovalDecision {
        self.wait_for_decision(core, request, stop).await.0
    }

    /// `wait_for_approval`, and whether the answer is the deadline's: an
    /// Autopilot task's approval nobody answered in `APPROVAL_DEADLINE` is
    /// denied (U03), which the log and a notice say.
    pub(super) async fn wait_for_decision(
        &self,
        core: &Core,
        request: ApprovalRequest,
        stop: &StopSignal,
    ) -> (ApprovalDecision, bool) {
        let (tx, rx) = oneshot::channel();
        let key = (request.dialog_id.clone(), request.tool_call_id.clone());
        let tool = request.tool_name.clone();
        let deadline = self
            .autopilot_of(&request.dialog_id)
            .map(|_| self.lock().approval_deadline.unwrap_or(APPROVAL_DEADLINE));
        self.lock().approvals.insert(
            key.clone(),
            Waiting {
                request: request.clone(),
                reply: tx,
            },
        );
        core.emit(AgentEvent::ApprovalRequested { request });
        // Stopped just before it was registered: nobody would answer.
        if stop.is_set() {
            self.resolve_approval(core, &key.0, &key.1, ApprovalDecision::Deny);
        }
        let Some(limit) = deadline else {
            return (rx.await.unwrap_or(ApprovalDecision::Deny), false);
        };
        let mut rx = rx;
        match tokio::time::timeout(limit, &mut rx).await {
            Ok(decision) => (decision.unwrap_or(ApprovalDecision::Deny), false),
            Err(_) => {
                #[cfg(test)]
                if ANSWER_AT_DEADLINE
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains(&key.0)
                {
                    self.resolve_approval(core, &key.0, &key.1, ApprovalDecision::Allow);
                }
                // An answer given as the deadline passed took the request
                // first: it stands. Else nobody answered: denied, and the
                // request goes from the chat.
                if !self.resolve_approval(core, &key.0, &key.1, ApprovalDecision::Deny) {
                    return (rx.await.unwrap_or(ApprovalDecision::Deny), false);
                }
                log::write(
                    core,
                    serde_json::json!({
                        "event": "approval",
                        "dialogId": key.0,
                        "toolCallId": key.1,
                        "tool": tool,
                        "outcome": "deadline",
                        "waitedSeconds": limit.as_secs(),
                    }),
                );
                core.emit(AgentEvent::Notice {
                    dialog_id: Some(key.0.clone()),
                    kind: "warning".into(),
                    title: "An approval wasn't answered".into(),
                    message: format!(
                        "Autopilot denied a {tool} call after waiting {} for an answer: nobody was there.",
                        crate::chat_request::human_duration(limit)
                    ),
                });
                (ApprovalDecision::Deny, true)
            }
        }
    }

    /// An Autopilot task's approvals are denied after `limit` from now on.
    #[cfg(test)]
    pub(super) fn set_approval_deadline(&self, limit: std::time::Duration) {
        self.lock().approval_deadline = Some(limit);
    }

    fn resolve_approval(
        &self,
        core: &Core,
        dialog_id: &str,
        tool_call_id: &str,
        decision: ApprovalDecision,
    ) -> bool {
        let key = (dialog_id.to_string(), tool_call_id.to_string());
        let Some(w) = self.lock().approvals.remove(&key) else {
            return false;
        };
        let _ = w.reply.send(decision);
        let (dialog_id, tool_call_id) = key;
        core.emit(AgentEvent::ApprovalSettled {
            dialog_id,
            tool_call_id,
        });
        true
    }

    /// Shows the questions and waits for the answer (Stopped if the run is stopped).
    pub async fn wait_for_answers(
        &self,
        core: &Core,
        request: QuestionsRequest,
        stop: &StopSignal,
    ) -> QuestionsOutcome {
        let (tx, rx) = oneshot::channel();
        let key = (request.dialog_id.clone(), request.tool_call_id.clone());
        self.lock().questions.insert(
            key.clone(),
            Waiting {
                request: request.clone(),
                reply: tx,
            },
        );
        core.emit(AgentEvent::QuestionsAsked { request });
        if stop.is_set() {
            self.answer_questions(
                core,
                &key.0,
                &key.1,
                QuestionsOutcome::Stopped { stopped: true },
            );
        }
        rx.await
            .unwrap_or(QuestionsOutcome::Stopped { stopped: true })
    }

    fn answer_questions(
        &self,
        core: &Core,
        dialog_id: &str,
        tool_call_id: &str,
        outcome: QuestionsOutcome,
    ) -> bool {
        let key = (dialog_id.to_string(), tool_call_id.to_string());
        let Some(w) = self.lock().questions.remove(&key) else {
            return false;
        };
        let _ = w.reply.send(outcome);
        let (dialog_id, tool_call_id) = key;
        core.emit(AgentEvent::QuestionsSettled {
            dialog_id,
            tool_call_id,
        });
        true
    }
}

/*
 * ---------------------------------------------------------------------------
 * Commands
 * ---------------------------------------------------------------------------
 */

/// Images and PDFs one message can carry at most.
const MAX_MESSAGE_ATTACHMENTS: usize = 20;

/// The settings runs follow, as the frontend sent them.
pub fn agent_configure(core: &Core, config: AgentConfig) {
    crate::overlay::chats::set_chat_resolves(config.permissions.chat_resolves_conflicts);
    core.agents.set_config(config);
}

/// A chat works in its own project only: its history, plan and changes are
/// about that one. Its run's folder must be one of that project's.
pub async fn check_project(
    db: &crate::chat_db::DbState,
    dialog_id: &str,
    root: Option<&str>,
) -> Result<(), String> {
    let Some(root) = root.filter(|r| !r.trim().is_empty()).map(str::to_string) else {
        return Ok(());
    };
    let id = dialog_id.to_string();
    let ok = db
        .read(move |c| {
            // T05: a candidate's chat works in a copy the IDE made of its
            // task chat's folder, which is no project's.
            let child = store::child_dialog(c, &id)?;
            if child.is_some_and(|(d, _)| d.kind == candidates::KIND) {
                return Ok(true);
            }
            let project = store::dialog_project(c, &id)?;
            crate::chat_db::projects::chat_may_use(
                c,
                project.as_deref(),
                std::path::Path::new(&root),
            )
        })
        .await?;
    if ok {
        Ok(())
    } else {
        Err("This chat belongs to another project. Open that project to continue it.".into())
    }
}

/// A message's files, once each, and only ones that are stored.
pub async fn check_attachments(
    db: &crate::chat_db::DbState,
    message: &mut UserMessage,
) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    message.attachments.retain(|id| seen.insert(id.clone()));
    if message.attachments.len() > MAX_MESSAGE_ATTACHMENTS {
        return Err(format!(
            "A message can have {MAX_MESSAGE_ATTACHMENTS} attachments at most; this one has {}.",
            message.attachments.len()
        ));
    }
    if message.attachments.is_empty() {
        return Ok(());
    }
    let ids = message.attachments.clone();
    let missing = db
        .read(move |c| crate::chat_db::attachments::missing(c, &ids))
        .await?;
    if missing.is_empty() {
        Ok(())
    } else {
        Err(ATTACHMENT_GONE.into())
    }
}

/// What a send says of a file it can no longer find.
pub const ATTACHMENT_GONE: &str =
    "A file attached to the message is no longer available. Remove it and attach it again.";

/// What a user message sends the model: the text with its editor context
/// (`meta.sentContent`), else its text.
pub fn wire_content(message: &UserMessage) -> String {
    run::sent_text(message.meta.as_ref()).unwrap_or_else(|| message.content.clone())
}

/// A sent message as the chat stores it.
pub fn stored_message(message: &UserMessage) -> NewMessage {
    NewMessage {
        id: message.id.clone(),
        role: "user".into(),
        content: message.content.clone(),
        tool_calls: None,
        tool_call_id: None,
        meta: message.meta.clone(),
        error: None,
        failed: false,
        attachments: message.attachments.clone(),
        created_at: store::now_ms(),
        finalized: true,
    }
}

/// How long a start waits for the chat's Autopilot driver between two
/// stages: a moment, but for a delivery that pushes the task's result.
const DRIVER_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Starts a run in the chat. Returns its id, or None when there was nothing
/// to do (a Resume with nothing left to run). A Resume of a chat whose
/// Autopilot task was cut goes on with the task (autopilot/).
pub async fn agent_start(core: &Core, request: StartRequest) -> Result<Option<String>, String> {
    if let Some(refusal) = mode_refusal(&request) {
        return Err(refusal);
    }
    if matches!(request.start, StartKind::Resume) {
        if let Some(config) = request.config.clone() {
            core.agents.set_config(config);
        }
        check_project(&core.db, &request.dialog_id, request.root.as_deref()).await?;
        if core.agents.draining() {
            return Err(RESTARTING.into());
        }
        // It takes the driver's turn itself, or finds it taken.
        if let Some(started) = autopilot::resume(core, &request).await? {
            return Ok(started);
        }
    }
    // A03: the chat's driver may be between two of its task's stages; the
    // start waits for its turn, so the two never interleave. The stage the
    // driver started is running then, and the start finds it.
    let _driving = autopilot::driver_turn(&request.dialog_id, DRIVER_WAIT).await?;
    start_run(core, request).await
}

/// Why a message's mode can't run on its model, if it can't; refused before
/// anything is stored. Autopilot's stages work with Rovik's tools (its plan
/// goes in with `submit_plan`, its subagents run through `task`), which a
/// model marked "no tools" or a connector running its own doesn't use. Plan
/// mode changes nothing, which a connector running its own tools promises
/// only with a plan mode of its own (`Connector::plan_mode`).
fn mode_refusal(request: &StartRequest) -> Option<String> {
    mode_refusal_with(request, |kind| {
        crate::connectors::connector(kind).plan_mode()
    })
}

/// `mode_refusal`, with whether a kind of connector has a plan mode of its
/// own given (tests: a kind without one).
fn mode_refusal_with(
    request: &StartRequest,
    plan_mode: impl Fn(crate::connectors::ConnectorKind) -> bool,
) -> Option<String> {
    let StartKind::Send { message } = &request.start else {
        return None;
    };
    if !request.profile.no_tools {
        return None;
    }
    let mode = message
        .meta
        .as_ref()
        .and_then(|m| m.get("mode"))
        .and_then(|m| m.as_str())
        .or(request.mode.as_deref());
    let kind = crate::connectors::kind_of(&request.profile.base_url);
    match mode {
        Some(autopilot::MODE) => Some(
            "Autopilot works with Rovik's tools, which this model doesn't use: pick one that does, or a connector set to use Rovik's tools (Settings → Connectors)."
                .into(),
        ),
        Some("plan") => kind
            .filter(|k| !plan_mode(*k))
            .map(|k| {
                format!(
                    "{} has no plan mode of its own, and its tools would change the project: use Agent mode, or set it to use Rovik's tools (Settings → Connectors).",
                    k.title()
                )
            }),
        _ => None,
    }
}

/// A start of a run, boxed (`start_run_boxed`).
pub(super) type Starting<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Option<String>, String>> + Send + 'a>,
>;

/// `start_run`, boxed: a run that starts runs of its own (T05's
/// candidates) can't name its own future's type otherwise.
pub(super) fn start_run_boxed(core: &Core, request: StartRequest) -> Starting<'_> {
    Box::pin(start_run(core, request))
}

/// Tests: the user answers Allow in this chat just as its approval's
/// deadline passes, before the deadline's Deny.
#[cfg(test)]
pub(crate) static ANSWER_AT_DEADLINE: Mutex<std::collections::BTreeSet<String>> =
    Mutex::new(std::collections::BTreeSet::new());

/// `agent_start`'s run, which the Autopilot driver starts its stages with.
pub(super) async fn start_run(
    core: &Core,
    request: StartRequest,
) -> Result<Option<String>, String> {
    let (state, db) = (&core.agents, &core.db);
    if let Some(config) = request.config.clone() {
        state.set_config(config);
    }
    let dialog_id = request.dialog_id.clone();
    check_project(db, &dialog_id, request.root.as_deref()).await?;
    // A message that starts an Autopilot task: its plan's record is written
    // before its run's start is told (`Run::prepare`), the driver at work
    // meanwhile.
    let _at_work = match &request.start {
        StartKind::Send { message } if autopilot::starts_task(message) => {
            Some(autopilot::AtWork::start(core, &dialog_id))
        }
        _ => None,
    };
    let mut request = request;
    if let StartKind::Send { message } = &mut request.start {
        check_attachments(db, message).await?;
        // "Implement this plan": the plan's text, from its file (A04).
        plan::expand_implementation(message, request.root.as_deref())?;
    }
    // A change to the chat's files outside any run (a server's merge)
    // ends first: the run starts on them as they're left.
    let controls = loop {
        state.files_let_go(&dialog_id).await;
        if state.draining() {
            return Err(RESTARTING.into());
        }
        if crate::chat_hold::elsewhere(&dialog_id) {
            return Err(crate::chat_hold::AT_WORK_ELSEWHERE.into());
        }
        match state.register(&dialog_id, &request.profile.id, request.options.clone()) {
            Some(controls) => break controls,
            // A drain began since the check above.
            None if state.draining() => return Err(RESTARTING.into()),
            // Held again since the wait: wait again.
            None if state.files_held(&dialog_id) => continue,
            None => return Err("The agent is already working in this chat.".into()),
        }
    };
    let run_id = controls.run_id.clone();
    // Registered until the run's end, whatever ends it: a start dropped or
    // panicking before its run is spawned would otherwise leave the chat
    // "already working" until the app restarts.
    let registered = state.registered(&dialog_id, &run_id);

    // A new message is stored before anything else: the run reads the
    // chat's history from the database.
    if let StartKind::Send { message } = &request.start {
        let new = stored_message(message);
        let id = dialog_id.clone();
        let stored = vec![new.clone()];
        match db
            .write(move |c| store::append_messages(c, &id, &stored))
            .await
        {
            Ok(seqs) => core.emit(AgentEvent::MessagesAdded {
                dialog_id: dialog_id.clone(),
                messages: vec![events::out_message(&dialog_id, seqs[0], &new, false)],
            }),
            Err(e) => {
                drop(registered);
                return Err(e);
            }
        }
    }

    // The user's message, which a run that can't start marks failed; the
    // IDE's own (an Autopilot stage's) stays as it is.
    let sent = match &request.start {
        StartKind::Send { message } if !run::is_harness_meta(message.meta.as_ref()) => {
            Some(message.id.clone())
        }
        _ => None,
    };
    // What a later stage of an Autopilot task starts its run with.
    let settings = autopilot::Settings::of(&request);
    let prepared = match run::Run::prepare(core, request, controls).await {
        Ok(Some(prepared)) => prepared,
        Ok(None) => {
            drop(registered);
            return Ok(None);
        }
        Err(e) => {
            drop(registered);
            // The message got no answer: it's marked failed, its draft comes back.
            if let Some(message_id) = sent {
                run::settle_failed(core, &dialog_id, message_id).await;
            }
            return Err(e);
        }
    };

    // An Agent-mode lead's run takes the messages the user sends it while
    // it works (steering-plan.md, D1), from now on.
    let steering = prepared.0.steer.clone();
    let steerable = match prepared.0.steering_refusal() {
        None => {
            steering.open();
            true
        }
        Some(why) => {
            steering.refuse(why);
            false
        }
    };
    core.emit(AgentEvent::RunStarted {
        dialog_id: dialog_id.clone(),
        run_id: run_id.clone(),
        steerable,
    });
    let id = run_id.clone();
    let core = core.clone();
    let unsettled = prepared.0.settle_message.clone();
    crate::rt::spawn(async move {
        let (mut run, entry) = prepared;
        let stage = run.stage.clone();
        // The run as a task of its own: should it panic, the chat is still
        // released and told, instead of reading "already working" until
        // the app restarts.
        let task = crate::rt::spawn(async move { run.go(entry).await });
        core.agents.set_abort(&dialog_id, &id, task.abort_handle());
        let outcome = match task.await {
            Ok(outcome) => outcome,
            // Cut where it only waited for the user (`cut_waiting`), as a
            // crash cuts it: its calls wait for a `Resume`.
            Err(e) if e.is_cancelled() => {
                let left = steering.close_and_take();
                hand_back_steering(&core, &dialog_id, left, "interrupted");
                crate::overlay::chats::set_running(&dialog_id, false);
                "interrupted".to_string()
            }
            Err(e) => {
                eprintln!("agent: run {id} in chat {dialog_id} ended abnormally: {e}");
                // Its subagents are tasks of their own: they stop with it.
                core.agents.stop_children(&core, &dialog_id);
                // What the user sent it that it didn't read goes back, and
                // a message it never answered is settled as failed.
                let left = steering.close_and_take();
                hand_back_steering(&core, &dialog_id, left, "error");
                if let Some(message_id) = run::take_unsettled(&unsettled) {
                    run::settle_failed(&core, &dialog_id, message_id).await;
                }
                "error".to_string()
            }
        };
        // An Autopilot stage's run: the driver's turn is taken before its
        // end is told, and the task goes on from here, its landing decided
        // (D2, A03); at work from before the end too, so its stage never
        // shows as cut short between the two.
        let driving = match &stage {
            Some(_) => Some((
                autopilot::drive_lock(&dialog_id).await,
                autopilot::AtWork::start(&core, &dialog_id),
            )),
            None => None,
        };
        // What it spent, for its task's budget (U04), and whether it was an
        // Autopilot task's run: told before it's gone.
        let spent = core.agents.run_spent(&dialog_id).unwrap_or_default();
        let autopilot = core.agents.autopilot_of(&dialog_id).is_some();
        drop(registered);
        core.emit(AgentEvent::RunEnded {
            dialog_id: dialog_id.clone(),
            run_id: id,
            outcome: outcome.clone(),
            autopilot,
            stage: stage.as_ref().map(|s| s.stage.id().to_string()),
        });
        if let (Some(stage), Some(driving)) = (stage, driving) {
            autopilot::after_run(core, dialog_id, stage, outcome, spent, settings, driving).await;
        }
    });
    Ok(Some(run_id))
}

/// A reasoning effort the user picked for a model (src/lib/reasoning.ts) in
/// a chat: runs using that model follow it. Returns how many runs it reached.
pub fn agent_set_effort(
    core: &Core,
    profile_id: String,
    options: Option<OpenAIChatOptions>,
    dialog_id: Option<String>,
) -> usize {
    core.agents
        .set_effort(&profile_id, options, dialog_id.as_deref())
}

/// How long a pause for a move waits for the chat's Autopilot driver to
/// be done with its turn: a moment between two stages, longer while it
/// delivers the task.
const PAUSE_DRIVER_WAIT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Pauses the chat's run for a move to `host` (6.8), or back to its
/// computer when `host` is empty: at its next safe point, where a
/// checkpoint pause happens, once its subagents are done, and with no
/// summary asked of the model (K29). Stop still stops it at once. Returns
/// when the run has ended: whether it paused for the move, which
/// `ContinueRun` then continues. False when no run went.
///
/// The chat's Autopilot driver between two of its task's stages starts
/// the next one's run in its turn: the pause waits for that turn to be
/// over, and the run it started pauses too, so none goes on after it.
pub async fn agent_pause(core: &Core, dialog_id: String, host: String) -> Result<bool, String> {
    let request = PauseRequest {
        kind: MOVE,
        host: Some(host).filter(|h| !h.is_empty()),
    };
    loop {
        if core.agents.is_driving(&dialog_id) {
            drop(autopilot::driver_turn(&dialog_id, PAUSE_DRIVER_WAIT).await?);
        }
        if !core.agents.pause(&dialog_id, request.clone()) {
            // No run of its own, but subagents a run that ended left
            // behind may still be writing its rows: they end before the
            // move takes them (K21), as Stop ends them.
            if core.agents.is_running(&dialog_id) {
                core.agents.stop_children(core, &dialog_id);
                ended(core, &dialog_id).await;
            }
            return Ok(false);
        }
        // A run that only waits for the user never reaches a safe point
        // while the chat moves (it's read-only meanwhile): it's cut, and
        // its calls wait for a `Resume` where the chat goes.
        if ended_or_cut(core, &dialog_id).await {
            return Ok(false);
        }
        if last_paused_for(core, &dialog_id, MOVE).await? {
            return Ok(true);
        }
        // It ended without seeing the pause, as it was ending anyway: its
        // driver, at work since, may start the task's next stage.
        if !core.agents.is_driving(&dialog_id) {
            return Ok(false);
        }
    }
}

/// The chat's Autopilot driver's turn, held (`hold_driver`): it starts no
/// stage until this is dropped.
pub struct DriverHeld {
    _turn: tokio::sync::OwnedMutexGuard<()>,
}

/// The chat's Autopilot driver's turn, once the driver is done with the
/// one it's at (recording a stage, starting the next: a moment), `wait`
/// at most, `DRIVER_BUSY` past it. For what mustn't interleave with the
/// driver starting a stage: a chat going back to its computer (6.8).
pub async fn hold_driver(dialog_id: &str, wait: std::time::Duration) -> Result<DriverHeld, String> {
    let turn = autopilot::driver_turn(dialog_id, wait).await?;
    Ok(DriverHeld { _turn: turn })
}

/// Pauses every run as the daemon stops, for an upgrade or a restart of
/// the server's Rovik (R02), each at its next safe point, as `agent_pause`
/// does; no run starts meanwhile. Returns once each has ended or waits
/// only for the user, or once `wait` has passed: how many paused for it,
/// which the next start of the daemon continues (5.4). What's left is cut
/// as a crash cuts it.
pub async fn drain(core: &Core, wait: std::time::Duration) -> usize {
    let chats = core.agents.drain();
    let until = std::time::Instant::now() + wait;
    let busy = |chat: &String| core.agents.is_running(chat) && !core.agents.waits_on_user(chat);
    while chats.iter().any(busy) && std::time::Instant::now() < until {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let mut paused = 0;
    for chat in &chats {
        if core.agents.is_running(chat) {
            continue;
        }
        if last_paused_for(core, chat, recovery::DRAIN)
            .await
            .unwrap_or(false)
        {
            paused += 1;
        }
    }
    paused
}

/// Waits until the chat's run and its subagents' have ended.
async fn ended(core: &Core, dialog_id: &str) {
    while core.agents.is_running(dialog_id) {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// `ended`, for a move's pause: a run that comes to wait only for the user
/// meanwhile is cut (`AgentState::cut_waiting`). True: it was cut.
async fn ended_or_cut(core: &Core, dialog_id: &str) -> bool {
    let mut cut = None;
    while core.agents.is_running(dialog_id) {
        if cut.is_none() {
            cut = core.agents.cut_waiting(core, dialog_id);
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    cut.is_some()
}

/// Takes back a move's pause asked of the chat's run: the move was
/// cancelled while it waited for the run's safe point.
pub fn agent_unpause_move(core: &Core, dialog_id: &str) {
    core.agents.unpause_move(dialog_id);
}

/// Whether the chat's last stored message is a pause of `kind`.
async fn last_paused_for(core: &Core, dialog_id: &str, kind: &str) -> Result<bool, String> {
    let id = dialog_id.to_string();
    let last = core
        .db
        .read(move |c| crate::chat_db::store::get_messages(c, &id, None, 1))
        .await?;
    let meta = last.messages.last().and_then(|m| m.meta.as_ref());
    let paused = meta.and_then(|meta| meta.pointer("/paused/kind")?.as_str());
    Ok(paused == Some(kind))
}

/// Whether the chat's last stored message is a pause for a move, which a
/// `ContinueRun` continues where the chat went (6.8).
pub async fn paused_for_move(core: &Core, dialog_id: &str) -> Result<bool, String> {
    last_paused_for(core, dialog_id, MOVE).await
}

/// Whether the chat's last step has tool calls without results, which a
/// `Resume` runs: a moved chat's run picks up so on its host (6.8).
pub async fn waits_for_results(core: &Core, dialog_id: &str) -> Result<bool, String> {
    let recent = run::recent_messages(&core.db, dialog_id).await?;
    Ok(!run::unanswered_tool_calls(&recent).is_empty())
}

/// Stop: ends the chat's run as soon as it can (its request is cancelled;
/// what it waits on is answered "no").
/// Stop: the chat's run ends as soon as it can. With no run to stop, the
/// stage of its Autopilot task that waits out the provider's rate limit
/// ends stopped (H07). Whether there was either.
pub async fn agent_stop(core: &Core, dialog_id: String) -> bool {
    if stop_now(core, &dialog_id).await {
        return true;
    }
    // The chat's Autopilot driver at work between two of its task's stages,
    // when nothing runs: the stage it starts is stopped once its turn is
    // over. A task it delivers meanwhile has nothing left to stop.
    if !core.agents.is_driving(&dialog_id) {
        return false;
    }
    drop(autopilot::drive_lock(&dialog_id).await);
    stop_now(core, &dialog_id).await
}

/// Stops the chat's run, or its task's stage that waits out the provider's
/// rate limit: whether either was there to stop.
async fn stop_now(core: &Core, dialog_id: &str) -> bool {
    core.agents.stop(core, dialog_id) || autopilot::stop_waiting(core, dialog_id).await
}

pub fn agent_resolve_approval(
    core: &Core,
    dialog_id: String,
    tool_call_id: String,
    decision: ApprovalDecision,
) -> bool {
    core.agents
        .resolve_approval(core, &dialog_id, &tool_call_id, decision)
}

pub fn agent_answer_questions(
    core: &Core,
    dialog_id: String,
    tool_call_id: String,
    outcome: QuestionsOutcome,
) -> bool {
    core.agents
        .answer_questions(core, &dialog_id, &tool_call_id, outcome)
}

/// A message for the chat's run while it works (steering-plan.md): it
/// waits for the run's next step, which reads it as part of the request
/// (`Queued`). `Ended` when no run goes there, it was stopped, or it
/// decided to end: the message is for the chat's next run. `Refused` when
/// the run takes none (Plan mode, an Autopilot task) or too many wait.
pub async fn agent_steer(
    core: &Core,
    dialog_id: String,
    mut message: UserMessage,
) -> Result<SteerReply, String> {
    check_attachments(&core.db, &mut message).await?;
    // Its files wait with it: their grace period starts again now, and the
    // cleanup keeps them while it waits (steer.rs).
    if !message.attachments.is_empty() {
        let ids = message.attachments.clone();
        core.db
            .write(move |c| crate::chat_db::attachments::renew(c, &ids))
            .await?;
    }
    // What the run waits on, read before its queue's lock is taken. A
    // queue's lock is taken under `AgentState`'s (agent_status), never the
    // other way round.
    let (queue, waits) = {
        let inner = core.agents.lock();
        let lead = inner
            .runs
            .get(&dialog_id)
            .filter(|r| r.parent.is_none() && !inner.candidates.contains_key(&dialog_id));
        let Some(run) = lead.filter(|r| !r.stop.is_set()) else {
            return Ok(SteerReply::Ended);
        };
        let approval = inner.approvals.keys().any(|(d, _)| *d == dialog_id);
        let subagents = inner
            .runs
            .values()
            .any(|r| r.parent.as_deref() == Some(dialog_id.as_str()));
        let waits = match (approval, subagents) {
            (true, _) => Some("approval"),
            (false, true) => Some("subagents"),
            _ => None,
        };
        (run.steer.clone(), waits)
    };
    let (id, chars, files) = (
        message.id.clone(),
        message.content.chars().count(),
        message.attachments.len(),
    );
    let reply = queue.push(message);
    if reply == SteerReply::Queued {
        let doing = waits.unwrap_or_else(|| queue.doing().id());
        steer::log_line(
            core,
            &dialog_id,
            &id,
            "queued",
            serde_json::json!({ "chars": chars, "attachments": files, "doing": doing }),
        );
    }
    Ok(reply)
}

/// Takes back a message sent with `agent_steer` that the chat's run hasn't
/// read yet: whether it was still waiting.
pub fn agent_unsteer(core: &Core, dialog_id: String, message_id: String) -> bool {
    let queue = {
        let inner = core.agents.lock();
        match inner.runs.get(&dialog_id).filter(|r| r.parent.is_none()) {
            Some(run) => run.steer.clone(),
            None => return false,
        }
    };
    let Some(taken) = queue.withdraw(&message_id) else {
        return false;
    };
    steer::log_line(
        core,
        &dialog_id,
        &message_id,
        "withdrawn",
        serde_json::json!({ "waitedMs": taken.waited_ms() }),
    );
    true
}

/// Messages a run held but didn't read go back to the user's input
/// (D15): told before the run's end, and logged.
pub(super) fn hand_back_steering(
    core: &Core,
    dialog_id: &str,
    left: Vec<steer::Queued>,
    outcome: &str,
) {
    if left.is_empty() {
        return;
    }
    for q in &left {
        steer::release(&q.message.attachments);
        steer::log_line(
            core,
            dialog_id,
            &q.message.id,
            "returned",
            serde_json::json!({ "waitedMs": q.waited_ms(), "why": outcome }),
        );
    }
    core.emit(AgentEvent::SteerReturned {
        dialog_id: dialog_id.to_string(),
        message_ids: left.into_iter().map(|q| q.message.id).collect(),
        outcome: outcome.to_string(),
    });
}

/// What's going on right now, for a frontend that (re)starts mid-run.
pub fn agent_status(core: &Core) -> AgentStatus {
    let inner = core.agents.lock();
    AgentStatus {
        // A subagent's chat isn't a chat in the list: its lead's run stands
        // for it (its blocks show it running). Nor is a candidate's (T05),
        // whose task chat's run stands for it.
        runs: inner
            .runs
            .iter()
            .filter(|(dialog_id, r)| {
                r.parent.is_none() && !inner.candidates.contains_key(*dialog_id)
            })
            .map(|(dialog_id, r)| RunningRun {
                dialog_id: dialog_id.clone(),
                run_id: r.run_id.clone(),
                steerable: r.steer.steerable(),
            })
            .collect(),
        subagents: inner
            .runs
            .iter()
            .filter_map(|(dialog_id, r)| {
                // T05: with its line's info, so a page opened meanwhile
                // shows how far it got, and its progress from there on.
                if let Some(c) = inner.candidates.get(dialog_id) {
                    return Some(RunningSubagent {
                        dialog_id: dialog_id.clone(),
                        parent_dialog_id: c.main.clone(),
                        parent_call_id: c.call.clone(),
                        info: Some(c.info.clone()),
                    });
                }
                Some(RunningSubagent {
                    dialog_id: dialog_id.clone(),
                    parent_dialog_id: r.parent.clone()?,
                    parent_call_id: r.parent_call.clone().unwrap_or_default(),
                    info: r.child.clone(),
                })
            })
            .collect(),
        approvals: inner
            .approvals
            .values()
            .map(|w| w.request.clone())
            .collect(),
        questions: inner
            .questions
            .values()
            .map(|w| w.request.clone())
            .collect(),
        // As with the runs: a candidate's chat isn't one in the list.
        driving: inner
            .driving
            .keys()
            .filter(|dialog_id| !inner.candidates.contains_key(*dialog_id))
            .cloned()
            .collect(),
        steering: inner
            .runs
            .iter()
            .filter(|(_, r)| r.parent.is_none())
            .flat_map(|(dialog_id, r)| r.steer.status(dialog_id))
            .collect(),
    }
}

/// The chat's current checklist, as the backend computes it
/// (`plan::current_checklist`, carried checklists included): the single
/// source of truth for the UI panel, instead of the UI recomputing it from
/// the messages it has loaded.
pub async fn chat_checklist(
    core: &Core,
    dialog_id: String,
) -> Result<Option<plan::CurrentChecklist>, String> {
    const RECENT: usize = 500;
    let page = core
        .db
        .read(move |c| store::get_messages(c, &dialog_id, None, RECENT))
        .await
        .map_err(|e| e.to_string())?;
    Ok(plan::current_checklist(&page.messages))
}

/// The checklist panel's dismiss ✕: marks the message holding the
/// checklist's `todo` call `checklistClosed` (its other meta kept), so
/// `plan::current_checklist` treats it as closed now and never carries it
/// into a later request. Returns the chat's checklist after that.
pub async fn chat_dismiss_checklist(
    core: &Core,
    dialog_id: String,
    message_id: String,
) -> Result<Option<plan::CurrentChecklist>, String> {
    let (id, chat) = (message_id.clone(), dialog_id.clone());
    let meta = core
        .db
        .write(move |c| {
            // The chat's own message: the request is allowed by the chat.
            let current: Option<String> = c
                .query_row(
                    "SELECT meta FROM messages WHERE id = ?1 AND dialog_id = ?2",
                    rusqlite::params![id, chat],
                    |r| r.get(0),
                )
                .map_err(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => {
                        "The message isn't in this chat.".to_string()
                    }
                    e => e.to_string(),
                })?;
            let mut meta = current
                .and_then(|m| serde_json::from_str::<serde_json::Value>(&m).ok())
                .filter(|m| m.is_object())
                .unwrap_or_else(|| serde_json::json!({}));
            meta["checklistClosed"] = serde_json::json!(true);
            let patch = store::MessagePatch {
                meta: Some(meta.clone()),
                ..Default::default()
            };
            store::update_message(c, &id, &patch)?;
            Ok(meta)
        })
        .await
        .map_err(|e| e.to_string())?;
    core.emit(events::AgentEvent::MessagePatched {
        dialog_id: dialog_id.clone(),
        message_id,
        patch: events::Patch {
            meta: Some(meta),
            ..Default::default()
        },
    });
    chat_checklist(core, dialog_id).await
}

#[cfg(test)]
mod run_control_tests {
    use super::StopSignal;

    /// A daemon's runs follow their computer's configuration (D16): the
    /// resolver's for the chat, a subagent's its lead's, and the one
    /// configuration for a chat the resolver doesn't know.
    #[test]
    fn a_chats_runs_follow_the_resolvers_configuration() {
        let state = super::AgentState::default();
        let mut desk = super::AgentConfig::default();
        desk.permissions.allow_any_command = true;
        assert!(!state.config_for("chat").permissions.allow_any_command);
        let resolved = desk.clone();
        state.set_resolver(std::sync::Arc::new(move |chat| {
            (chat == "chat").then(|| resolved.clone())
        }));
        assert_eq!(state.config_for("chat"), desk);
        state.register_in("chat", None, "m", None).unwrap();
        state
            .register_child("kid", "chat", "t1", "m", None)
            .unwrap();
        assert_eq!(state.config_for("kid"), desk, "the lead's");
        assert_eq!(state.config_for("other"), state.config());
    }

    /// An effort picked on one computer reaches its own chats' runs only.
    #[test]
    fn an_effort_reaches_only_the_chats_it_is_for() {
        let state = super::AgentState::default();
        let mine = state.register("effort-mine", "m", None).unwrap();
        let theirs = state.register("effort-theirs", "m", None).unwrap();
        let reached = state.set_effort_where("m", None, None, |chat| chat == "effort-mine");
        assert_eq!(reached, 1);
        assert!(mine.effort.has_changed().unwrap());
        assert!(!theirs.effort.has_changed().unwrap());
    }

    /// No lead's run registers once a drain has begun, whenever its start
    /// checked; a subagent of a run still going does (its lead finishes).
    #[test]
    fn no_run_registers_once_a_drain_began() {
        let state = super::AgentState::default();
        state.register("drain-lead", "m", None).unwrap();
        state.drain();
        assert!(state.register("drain-late", "m", None).is_none());
        assert!(state
            .register_child("drain-kid", "drain-lead", "t1", "m", None)
            .is_some());
    }

    #[test]
    fn a_stop_set_before_anyone_listens_is_seen_by_later_listeners() {
        let stop = StopSignal::new();
        stop.set();
        assert!(*stop.subscribe().borrow());
    }

    #[tokio::test]
    async fn a_run_that_panics_is_unregistered() {
        let state = super::AgentState::default();
        let controls = state.register_in("kid", None, "m", None).unwrap();
        let guard = state.registered("kid", &controls.run_id);
        let task = tokio::spawn(async move {
            let _guard = guard;
            panic!("the run broke");
        });
        assert!(task.await.is_err());
        assert!(!state.lock().runs.contains_key("kid"));
        // A new run in the chat can start.
        assert!(state.register_in("kid", None, "m", None).is_some());
    }

    /// Round 2 (§4.2): a carry past its time is dropped when it's taken,
    /// and its spilled files with it.
    #[test]
    fn a_carry_past_its_ttl_is_dropped_on_take() {
        use super::verify::{VerifyCarry, CARRY_TTL};
        let state = super::AgentState::default();
        let file = std::env::temp_dir().join(format!("carry-{}", uuid::Uuid::new_v4()));
        std::fs::write(&file, "x").unwrap();
        let old = CARRY_TTL + std::time::Duration::from_secs(1);
        state.put_carry(
            "chat",
            VerifyCarry::for_test("rec", old, vec![file.clone()]),
        );
        assert!(state.take_carry("chat", "rec").is_none());
        assert!(!file.exists(), "its spilled file went");
        // Within its time it's taken.
        state.put_carry(
            "chat",
            VerifyCarry::for_test("rec", std::time::Duration::ZERO, vec![]),
        );
        assert!(state.take_carry("chat", "rec").is_some());
    }

    /// Round 2 (§4.2): a newer carry replaces the chat's older one, whose
    /// spilled files go; another record's take leaves it in place.
    #[test]
    fn a_newer_carry_replaces_and_removes_the_old_spill_files() {
        use super::verify::VerifyCarry;
        let state = super::AgentState::default();
        let file = std::env::temp_dir().join(format!("carry-{}", uuid::Uuid::new_v4()));
        std::fs::write(&file, "x").unwrap();
        let now = std::time::Duration::ZERO;
        state.put_carry(
            "chat",
            VerifyCarry::for_test("old", now, vec![file.clone()]),
        );
        state.put_carry("chat", VerifyCarry::for_test("new", now, vec![]));
        assert!(!file.exists());
        assert!(state.take_carry("chat", "old").is_none());
        assert!(state.has_carry("chat"), "an older record's take leaves it");
        assert!(state.take_carry("chat", "new").is_some());
    }

    /// 6.7: a hold on a chat's files is refused while its run goes, and
    /// while held, no run of the chat registers; let go, one does.
    #[test]
    fn a_chats_files_held_keep_its_runs_from_starting() {
        let state = super::AgentState::default();
        let run = state.register_in("chat", None, "m", None).unwrap();
        let refused = state.hold_files("chat").err();
        assert_eq!(refused, Some(super::NotHeld::Running), "its run goes");
        state.unregister("chat", &run.run_id);
        let held = state.hold_files("chat").unwrap();
        let refused = state.hold_files("chat").err();
        assert_eq!(refused, Some(super::NotHeld::Held), "held once");
        assert!(state.register_in("chat", None, "m", None).is_none());
        assert!(state.files_held("chat"));
        drop(held);
        assert!(!state.files_held("chat"));
        assert!(state.register_in("chat", None, "m", None).is_some());
    }

    /// 6.7: a start waits for the hold to go.
    #[tokio::test]
    async fn a_start_waits_for_the_chats_files_to_be_let_go() {
        let state = super::AgentState::default();
        let held = state.hold_files("chat").unwrap();
        let waiting = {
            let state = state.clone();
            tokio::spawn(async move { state.files_let_go("chat").await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "held");
        drop(held);
        tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
            .await
            .expect("let go")
            .unwrap();
    }

    /// Round 2 (section 4.2): run A's guard, dropped after run B started in
    /// the same chat, leaves B running.
    #[test]
    fn unregister_ignores_a_newer_run_with_the_same_dialog() {
        let state = super::AgentState::default();
        let a = state.register_in("chat", None, "m", None).unwrap();
        let guard_a = state.registered("chat", &a.run_id);
        state.unregister("chat", &a.run_id);
        let b = state.register_in("chat", None, "m", None).unwrap();
        drop(guard_a);
        assert!(state.is_running("chat"));
        assert_eq!(state.lock().runs["chat"].run_id, b.run_id);
    }
}

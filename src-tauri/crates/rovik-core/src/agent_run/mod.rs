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
//!   repair.rs     its fix rounds: progress, rollback, test integrity
//!   guard.rs      checkpoints and loop detection
//!   subagent.rs, subagent_run.rs: helper runs the lead starts (`task`)
//!   change_review.rs  a fresh-context review of the change before the checks
//!   tools.rs      tool schemas and running a tool
//!   permissions.rs, plan.rs, project.rs, repetition.rs, diff.rs,
//!   command_output.rs: the rules and text they use

pub mod causes;
pub mod change_review;
pub mod check_output;
pub mod command_output;
pub mod diff;
#[cfg(test)]
mod e2e_tests;
pub mod events;
mod execute;
pub mod failure;
pub mod group;
pub mod guard;
mod landing;
pub mod log;
mod parallel;
pub mod paths;
pub mod permissions;
pub mod plan;
pub mod plan_review;
pub mod project;
pub mod recovery;
pub mod repair;
mod repair_run;
pub mod repetition;
pub mod review;
mod run;
pub mod server;
pub mod subagent;
mod subagent_run;
pub mod test_writer;
pub mod tools;
pub mod triage;
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

pub use landing::{checks_would_run, pending_reason};
pub use rovik_protocol::agent::{
    AgentConfig, AgentStatus, ApprovalDecision, ChildInfo, ModelProfile, RunningRun,
    RunningSubagent, StartKind, StartRequest, UserMessage,
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

    fn set(&self, request: PauseRequest) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(request);
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
    /// A subagent's: what its block shows, for a page reloaded mid-run (P1.5).
    child: Option<ChildInfo>,
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
    runs: HashMap<String, RunControl>,
    /// Keyed by chat and tool call: providers number calls per reply
    /// (Gemini: "function-call-1"), so two chats can wait on the same id.
    approvals: HashMap<WaitKey, Waiting<ApprovalDecision, ApprovalRequest>>,
    questions: HashMap<WaitKey, Waiting<QuestionsOutcome, QuestionsRequest>>,
    /// A run's fix-round state while its checks wait for the user's
    /// approval, by chat (verify.rs `VerifyCarry`).
    carries: HashMap<String, verify::VerifyCarry>,
}

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
    /// follows its lead's.
    pub fn config_for(&self, dialog_id: &str) -> AgentConfig {
        let (resolver, lead) = {
            let inner = self.lock();
            let mut chat = dialog_id;
            while let Some(parent) = inner.runs.get(chat).and_then(|r| r.parent.as_deref()) {
                chat = parent;
            }
            (inner.resolver.clone(), chat.to_string())
        };
        // Asked without the lock: the resolver may take locks of its own.
        resolver
            .and_then(|resolve| resolve(&lead))
            .unwrap_or_else(|| self.config())
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

    /// Registers a run for the chat; None when one is already going there.
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
        let mut inner = self.lock();
        if inner.runs.contains_key(dialog_id) {
            return None;
        }
        let run_id = uuid::Uuid::new_v4().to_string();
        let stop = Arc::new(StopSignal::new());
        let pause = Arc::new(PauseSignal::default());
        let (effort_tx, effort_rx) = watch::channel(EffortPick {
            options,
            here: false,
        });
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
                child: None,
            },
        );
        Some(Controls {
            run_id,
            stop,
            pause,
            effort: effort_rx,
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

    /// Asks every lead's run to pause for a drain: their chats.
    pub fn drain(&self) -> Vec<String> {
        let inner = self.lock();
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
        let inner = self.lock();
        let mut reached = 0;
        for (dialog, run) in inner
            .runs
            .iter()
            .filter(|(_, r)| r.profile_id == profile_id && r.parent.is_none())
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

    /// A subagent's progress, as its block's event says.
    pub(super) fn note_child_progress(&self, dialog_id: &str, tool_calls: u32, model: &str) {
        if let Some(info) = self
            .lock()
            .runs
            .get_mut(dialog_id)
            .and_then(|r| r.child.as_mut())
        {
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

    /// Whether the chat has a run going (a lead's subagents count as its own).
    pub fn is_running(&self, dialog_id: &str) -> bool {
        let inner = self.lock();
        inner.runs.contains_key(dialog_id)
            || inner
                .runs
                .values()
                .any(|r| r.parent.as_deref() == Some(dialog_id))
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
        let children: Vec<String> = inner
            .runs
            .iter()
            .filter(|(_, r)| r.parent.as_deref() == Some(dialog_id))
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
        for key in approvals {
            if let Some(w) = inner.approvals.remove(&key) {
                let _ = w.reply.send(ApprovalDecision::Deny);
                let (dialog_id, tool_call_id) = key;
                core.emit(AgentEvent::ApprovalSettled {
                    dialog_id,
                    tool_call_id,
                });
            }
        }
        let questions: Vec<WaitKey> = inner
            .questions
            .keys()
            .filter(|(d, _)| stopping(d))
            .cloned()
            .collect();
        for key in questions {
            if let Some(w) = inner.questions.remove(&key) {
                let _ = w.reply.send(QuestionsOutcome::Stopped { stopped: true });
                let (dialog_id, tool_call_id) = key;
                core.emit(AgentEvent::QuestionsSettled {
                    dialog_id,
                    tool_call_id,
                });
            }
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
        let (tx, rx) = oneshot::channel();
        let key = (request.dialog_id.clone(), request.tool_call_id.clone());
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
        rx.await.unwrap_or(ApprovalDecision::Deny)
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

/// Starts a run in the chat. Returns its id, or None when there was nothing
/// to do (a Resume with nothing left to run).
pub async fn agent_start(core: &Core, request: StartRequest) -> Result<Option<String>, String> {
    let (state, db) = (&core.agents, &core.db);
    if let Some(config) = request.config.clone() {
        state.set_config(config);
    }
    let dialog_id = request.dialog_id.clone();
    check_project(db, &dialog_id, request.root.as_deref()).await?;
    let mut request = request;
    if let StartKind::Send { message } = &mut request.start {
        check_attachments(db, message).await?;
    }
    let Some(controls) = state.register(&dialog_id, &request.profile.id, request.options.clone())
    else {
        return Err("The agent is already working in this chat.".into());
    };
    let run_id = controls.run_id.clone();

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
                state.unregister(&dialog_id, &run_id);
                return Err(e);
            }
        }
    }

    let sent = match &request.start {
        StartKind::Send { message } => Some(message.id.clone()),
        _ => None,
    };
    let prepared = match run::Run::prepare(core, request, controls).await {
        Ok(Some(prepared)) => prepared,
        Ok(None) => {
            state.unregister(&dialog_id, &run_id);
            return Ok(None);
        }
        Err(e) => {
            state.unregister(&dialog_id, &run_id);
            // The message got no answer: it's marked failed, its draft comes back.
            if let Some(message_id) = sent {
                let id = message_id.clone();
                let patch = store::MessagePatch {
                    failed: Some(true),
                    ..Default::default()
                };
                if db
                    .write(move |c| store::update_message(c, &id, &patch))
                    .await
                    .is_ok()
                {
                    core.emit(AgentEvent::MessagePatched {
                        dialog_id: dialog_id.clone(),
                        message_id: message_id.clone(),
                        patch: events::Patch {
                            failed: Some(true),
                            ..Default::default()
                        },
                    });
                }
                core.emit(AgentEvent::UserMessageSettled {
                    dialog_id,
                    message_id,
                    ok: false,
                });
            }
            return Err(e);
        }
    };

    core.emit(AgentEvent::RunStarted {
        dialog_id: dialog_id.clone(),
        run_id: run_id.clone(),
    });
    let id = run_id.clone();
    let core = core.clone();
    crate::rt::spawn(async move {
        let (mut run, entry) = prepared;
        // The run as a task of its own: should it panic, the chat is still
        // released and told, instead of reading "already working" until
        // the app restarts.
        let outcome = match crate::rt::spawn(async move { run.go(entry).await }).await {
            Ok(outcome) => outcome,
            Err(e) => {
                eprintln!("agent: run {id} in chat {dialog_id} ended abnormally: {e}");
                "error".to_string()
            }
        };
        core.agents.unregister(&dialog_id, &id);
        core.emit(AgentEvent::RunEnded {
            dialog_id,
            run_id: id,
            outcome,
        });
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

/// Pauses the chat's run for a move to `host` (6.8), or back to its
/// computer when `host` is empty: at its next safe point, where a
/// checkpoint pause happens, once its subagents are done, and with no
/// summary asked of the model (K29). Stop still stops it at once. Returns
/// when the run has ended: whether it paused for the move, which
/// `ContinueRun` then continues. False when no run went.
pub async fn agent_pause(core: &Core, dialog_id: String, host: String) -> Result<bool, String> {
    let request = PauseRequest {
        kind: MOVE,
        host: Some(host).filter(|h| !h.is_empty()),
    };
    if !core.agents.pause(&dialog_id, request) {
        return Ok(false);
    }
    ended(core, &dialog_id).await;
    last_paused_for(core, &dialog_id, MOVE).await
}

/// Pauses every run for an upgrade of the server's Rovik (R02), each at
/// its next safe point, as `agent_pause` does. Returns once all of them
/// have ended: how many paused for it, which the new binary continues
/// (5.4).
pub async fn drain(core: &Core) -> usize {
    let mut paused = 0;
    for chat in core.agents.drain() {
        ended(core, &chat).await;
        if last_paused_for(core, &chat, recovery::DRAIN)
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
pub fn agent_stop(core: &Core, dialog_id: String) -> bool {
    core.agents.stop(core, &dialog_id)
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

/// What's going on right now, for a frontend that (re)starts mid-run.
pub fn agent_status(core: &Core) -> AgentStatus {
    let inner = core.agents.lock();
    AgentStatus {
        // A subagent's chat isn't a chat in the list: its lead's run stands
        // for it (its blocks show it running).
        runs: inner
            .runs
            .iter()
            .filter(|(_, r)| r.parent.is_none())
            .map(|(dialog_id, r)| RunningRun {
                dialog_id: dialog_id.clone(),
                run_id: r.run_id.clone(),
            })
            .collect(),
        subagents: inner
            .runs
            .iter()
            .filter_map(|(dialog_id, r)| {
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
    let id = message_id.clone();
    let meta = core
        .db
        .write(move |c| {
            let current: Option<String> = c
                .query_row(
                    "SELECT meta FROM messages WHERE id = ?1",
                    rusqlite::params![id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
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

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
//!   guard.rs      checkpoints and loop detection
//!   subagent.rs, subagent_run.rs: helper runs the lead starts (`task`)
//!   tools.rs      tool schemas and running a tool
//!   permissions.rs, plan.rs, project.rs, repetition.rs, diff.rs,
//!   command_output.rs: the rules and text they use

pub mod command_output;
pub mod diff;
#[cfg(test)]
mod e2e_tests;
pub mod events;
mod execute;
pub mod failure;
pub mod guard;
mod landing;
pub mod log;
mod parallel;
pub mod permissions;
pub mod plan;
pub mod project;
pub mod repetition;
pub mod review;
mod run;
pub mod subagent;
mod subagent_run;
pub mod tools;
mod verify;

use crate::agent::OpenAIChatOptions;
use crate::chat_db::store::{self, NewMessage};
use crate::chat_db::DbState;
use events::{emit, AgentEvent, ApprovalRequest, OutMessage, QuestionsRequest};
use permissions::Permissions;
use plan::QuestionsOutcome;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Runtime, State};
use tokio::sync::{oneshot, watch};

/// A model, resolved by the frontend from the providers in its settings.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ModelProfile {
    /// `${providerId}::${modelId}`.
    pub id: String,
    /// "Provider/developer/model", for the user.
    pub label: String,
    pub model: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub vision: bool,
    pub context_tokens: Option<usize>,
    #[serde(default)]
    pub no_tools: bool,
    pub first_byte_timeout_ms: Option<u64>,
    /// The API route saved with the model ("chat_completions" or
    /// "responses"), once found (`chat_request::responses`).
    #[serde(default)]
    pub api_route: Option<String>,
}

/// The settings runs follow, sent by the frontend at startup and whenever
/// they change; a running run picks changes up at its next tool call.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentConfig {
    pub permissions: Permissions,
    /// The AI reviewer, then its fallbacks (enabled models only).
    pub approval_profiles: Vec<ModelProfile>,
    /// Subagents (subagent.rs): off unless the user turns them on.
    pub subagents: subagent::SubagentConfig,
}

/// The user's decision on a tool call waiting for approval.
#[derive(Deserialize, Clone, Debug)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ApprovalDecision {
    Allow,
    Deny,
    /// Some of an edit's changes accepted: the file with only those.
    Partial {
        content: String,
        total: usize,
        rejected: Vec<diff::Hunk>,
    },
}

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
        let _ = self.tx.send(true);
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<bool> {
        self.tx.subscribe()
    }
}

struct RunControl {
    run_id: String,
    /// A subagent's run: its lead's chat and `task` call. Stop there stops it too.
    parent: Option<String>,
    parent_call: Option<String>,
    /// The run's model (`ModelProfile::id`): an effort chosen for it applies.
    profile_id: String,
    stop: Arc<StopSignal>,
    /// The reasoning effort picked while the run goes on (see `set_effort`).
    effort: watch::Sender<EffortPick>,
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
    pub effort: watch::Receiver<EffortPick>,
}

struct Waiting<T, R> {
    request: R,
    reply: oneshot::Sender<T>,
}

/// (dialog id, tool call id)
type WaitKey = (String, String);

#[derive(Default)]
struct Inner {
    config: AgentConfig,
    runs: HashMap<String, RunControl>,
    /// Keyed by chat and tool call: providers number calls per reply
    /// (Gemini: "function-call-1"), so two chats can wait on the same id.
    approvals: HashMap<WaitKey, Waiting<ApprovalDecision, ApprovalRequest>>,
    questions: HashMap<WaitKey, Waiting<QuestionsOutcome, QuestionsRequest>>,
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
                effort: effort_tx,
            },
        );
        Some(Controls {
            run_id,
            stop,
            effort: effort_rx,
        })
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
    pub fn stop<R: Runtime>(&self, app: &AppHandle<R>, dialog_id: &str) -> bool {
        let mut inner = self.lock();
        let Some(run) = inner.runs.get(dialog_id) else {
            return false;
        };
        run.stop.set();
        let children: Vec<String> = inner
            .runs
            .iter()
            .filter(|(_, r)| r.parent.as_deref() == Some(dialog_id))
            .map(|(d, r)| {
                r.stop.set();
                d.clone()
            })
            .collect();
        let stopping = |d: &String| d == dialog_id || children.contains(d);
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
                emit(
                    app,
                    AgentEvent::ApprovalSettled {
                        dialog_id,
                        tool_call_id,
                    },
                );
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
                emit(
                    app,
                    AgentEvent::QuestionsSettled {
                        dialog_id,
                        tool_call_id,
                    },
                );
            }
        }
        true
    }

    /// Asks the user and waits for the decision (Deny if the run is stopped).
    pub async fn wait_for_approval<R: Runtime>(
        &self,
        app: &AppHandle<R>,
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
        emit(app, AgentEvent::ApprovalRequested { request });
        // Stopped just before it was registered: nobody would answer.
        if stop.is_set() {
            self.resolve_approval(app, &key.0, &key.1, ApprovalDecision::Deny);
        }
        rx.await.unwrap_or(ApprovalDecision::Deny)
    }

    fn resolve_approval<R: Runtime>(
        &self,
        app: &AppHandle<R>,
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
        emit(
            app,
            AgentEvent::ApprovalSettled {
                dialog_id,
                tool_call_id,
            },
        );
        true
    }

    /// Shows the questions and waits for the answer (Stopped if the run is stopped).
    pub async fn wait_for_answers<R: Runtime>(
        &self,
        app: &AppHandle<R>,
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
        emit(app, AgentEvent::QuestionsAsked { request });
        if stop.is_set() {
            self.answer_questions(
                app,
                &key.0,
                &key.1,
                QuestionsOutcome::Stopped { stopped: true },
            );
        }
        rx.await
            .unwrap_or(QuestionsOutcome::Stopped { stopped: true })
    }

    fn answer_questions<R: Runtime>(
        &self,
        app: &AppHandle<R>,
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
        emit(
            app,
            AgentEvent::QuestionsSettled {
                dialog_id,
                tool_call_id,
            },
        );
        true
    }
}

/*
 * ---------------------------------------------------------------------------
 * Commands
 * ---------------------------------------------------------------------------
 */

/// How a run starts.
#[derive(Deserialize, Debug)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum StartKind {
    /// A new message from the user (stored here, first).
    Send { message: UserMessage },
    /// Another try after a failed turn.
    Retry,
    /// Continue an interrupted reply.
    ContinueReply,
    /// Continue a run that paused (a checkpoint or a loop check).
    ContinueRun,
    /// Run the checks of a verification record that waited for approval.
    RunChecks { record_id: String },
    /// A chat whose last tool calls never ran (the app closed meanwhile).
    Resume,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UserMessage {
    pub id: String,
    pub content: String,
    #[serde(default)]
    pub meta: Option<serde_json::Value>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StartRequest {
    pub dialog_id: String,
    pub start: StartKind,
    pub profile: ModelProfile,
    /// The reasoning effort, in the forms the provider takes (src/lib/reasoning.ts).
    pub options: Option<OpenAIChatOptions>,
    /// The mode selected in the chat, for a run whose message didn't record one.
    #[serde(default)]
    pub mode: Option<String>,
    /// "trimming" or "clm".
    #[serde(default)]
    pub context_mode: Option<String>,
    pub root: Option<String>,
    /// The request's user message as sent (with editor context), when it
    /// isn't the stored one.
    pub current_user_content: Option<String>,
    /// Today, as the user's clock has it ("2026-10-04"), for plan file names.
    pub local_date: Option<String>,
    pub config: Option<AgentConfig>,
    /// Work in the chat's overlay (src/overlay). `None`: the app's setting.
    #[serde(default)]
    pub isolate: Option<bool>,
}

#[tauri::command]
pub fn agent_configure(state: State<'_, AgentState>, config: AgentConfig) {
    crate::overlay::chats::set_chat_resolves(config.permissions.chat_resolves_conflicts);
    state.set_config(config);
}

/// Starts a run in the chat. Returns its id, or None when there was nothing
/// to do (a Resume with nothing left to run).
#[tauri::command]
pub async fn agent_start<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AgentState>,
    db: State<'_, DbState>,
    request: StartRequest,
) -> Result<Option<String>, String> {
    if let Some(config) = request.config.clone() {
        state.set_config(config);
    }
    let dialog_id = request.dialog_id.clone();
    // A chat works in its own project only: its history, plan and changes
    // are about that one.
    if let Some(root) = request.root.clone().filter(|r| !r.trim().is_empty()) {
        let id = dialog_id.clone();
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
        if !ok {
            return Err(
                "This chat belongs to another project. Open that project to continue it.".into(),
            );
        }
    }
    let Some(controls) = state.register(&dialog_id, &request.profile.id, request.options.clone())
    else {
        return Err("The agent is already working in this chat.".into());
    };
    let run_id = controls.run_id.clone();
    let state = state.inner().clone();
    let db = db.inner().clone();

    // A new message is stored before anything else: the run reads the
    // chat's history from the database.
    if let StartKind::Send { message } = &request.start {
        let new = NewMessage {
            id: message.id.clone(),
            role: "user".into(),
            content: message.content.clone(),
            tool_calls: None,
            tool_call_id: None,
            meta: message.meta.clone(),
            error: None,
            failed: false,
            attachments: Vec::new(),
            created_at: store::now_ms(),
            finalized: true,
        };
        let id = dialog_id.clone();
        let stored = vec![new.clone()];
        match db
            .write(move |c| store::append_messages(c, &id, &stored))
            .await
        {
            Ok(seqs) => emit(
                &app,
                AgentEvent::MessagesAdded {
                    dialog_id: dialog_id.clone(),
                    messages: vec![OutMessage::from_new(&dialog_id, seqs[0], &new, false)],
                },
            ),
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
    let prepared = match run::Run::prepare(&app, &state, &db, request, controls).await {
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
                    emit(
                        &app,
                        AgentEvent::MessagePatched {
                            dialog_id: dialog_id.clone(),
                            message_id: message_id.clone(),
                            patch: events::Patch {
                                failed: Some(true),
                                ..Default::default()
                            },
                        },
                    );
                }
                emit(
                    &app,
                    AgentEvent::UserMessageSettled {
                        dialog_id,
                        message_id,
                        ok: false,
                    },
                );
            }
            return Err(e);
        }
    };

    emit(
        &app,
        AgentEvent::RunStarted {
            dialog_id: dialog_id.clone(),
            run_id: run_id.clone(),
        },
    );
    let id = run_id.clone();
    tauri::async_runtime::spawn(async move {
        let (mut run, entry) = prepared;
        let outcome = run.go(entry).await;
        state.unregister(&dialog_id, &id);
        emit(
            &app,
            AgentEvent::RunEnded {
                dialog_id,
                run_id: id,
                outcome,
            },
        );
    });
    Ok(Some(run_id))
}

/// A reasoning effort the user picked for a model (src/lib/reasoning.ts) in
/// a chat: runs using that model follow it. Returns how many runs it reached.
#[tauri::command]
pub fn agent_set_effort(
    state: State<'_, AgentState>,
    profile_id: String,
    options: Option<OpenAIChatOptions>,
    dialog_id: Option<String>,
) -> usize {
    state.set_effort(&profile_id, options, dialog_id.as_deref())
}

/// Stop: ends the chat's run as soon as it can (its request is cancelled;
/// what it waits on is answered "no").
#[tauri::command]
pub fn agent_stop<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AgentState>,
    dialog_id: String,
) -> bool {
    state.stop(&app, &dialog_id)
}

#[tauri::command]
pub fn agent_resolve_approval<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AgentState>,
    dialog_id: String,
    tool_call_id: String,
    decision: ApprovalDecision,
) -> bool {
    state.resolve_approval(&app, &dialog_id, &tool_call_id, decision)
}

#[tauri::command]
pub fn agent_answer_questions<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AgentState>,
    dialog_id: String,
    tool_call_id: String,
    outcome: QuestionsOutcome,
) -> bool {
    state.answer_questions(&app, &dialog_id, &tool_call_id, outcome)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningRun {
    dialog_id: String,
    run_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningSubagent {
    dialog_id: String,
    parent_dialog_id: String,
    parent_call_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    runs: Vec<RunningRun>,
    subagents: Vec<RunningSubagent>,
    approvals: Vec<ApprovalRequest>,
    questions: Vec<QuestionsRequest>,
}

/// What's going on right now, for a frontend that (re)starts mid-run.
#[tauri::command]
pub fn agent_status(state: State<'_, AgentState>) -> AgentStatus {
    let inner = state.lock();
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
#[tauri::command]
pub async fn chat_checklist(
    db: State<'_, DbState>,
    dialog_id: String,
) -> Result<Option<plan::CurrentChecklist>, String> {
    const RECENT: usize = 500;
    let page = db
        .read(move |c| store::get_messages(c, &dialog_id, None, RECENT))
        .await
        .map_err(|e| e.to_string())?;
    Ok(plan::current_checklist(&page.messages))
}

/// The checklist panel's dismiss ✕: marks the message holding the
/// checklist's `todo` call `checklistClosed` (its other meta kept), so
/// `plan::current_checklist` treats it as closed now and never carries it
/// into a later request. Returns the chat's checklist after that.
#[tauri::command]
pub async fn chat_dismiss_checklist<R: Runtime>(
    app: AppHandle<R>,
    db: State<'_, DbState>,
    dialog_id: String,
    message_id: String,
) -> Result<Option<plan::CurrentChecklist>, String> {
    let id = message_id.clone();
    let meta = db
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
    events::emit(
        &app,
        events::AgentEvent::MessagePatched {
            dialog_id: dialog_id.clone(),
            message_id,
            patch: events::Patch {
                meta: Some(meta),
                ..Default::default()
            },
        },
    );
    chat_checklist(db, dialog_id).await
}

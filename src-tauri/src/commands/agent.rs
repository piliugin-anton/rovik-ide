//! The agent's runs (agent_run), and the requests the frontend makes itself
//! (agent.rs). A chat on a host runs there (6.1).

use rovik_client::desktop_api::agent::{AgentRequest, CompletionArgs, EstimateArgs};
use rovik_client::desktop_connection::DesktopConnection;
use rovik_core::agent::{OpenAIChatOptions, ReasoningEffort, WireChatMessage};
use rovik_core::agent_run::checklist::CurrentChecklist;
use rovik_core::agent_run::questions::QuestionsOutcome;
use rovik_core::agent_run::{
    AgentConfig, AgentStatus, ApprovalDecision, StartRequest, SteerReply, UserMessage,
};
use rovik_protocol::connectors::{
    ConnectorCommand, ConnectorProbe, ProbeRequestArgs, SetupJob, SetupOp,
};
use rovik_protocol::events::BackgroundTask;
use rovik_protocol::Request;
use tauri::ipc::Channel;
use tauri::State;

/// Here and on every connected host (6.1).
#[tauri::command]
pub async fn agent_configure(
    backend: State<'_, DesktopConnection>,
    config: AgentConfig,
) -> Result<(), String> {
    backend
        .call(Request::AgentConfigure(Box::new(config)))
        .await
}

/// Starts a run in the chat. Returns its id, or None when there was nothing
/// to do (a Resume with nothing left to run).
#[tauri::command]
pub async fn agent_start(
    backend: State<'_, DesktopConnection>,
    request: StartRequest,
) -> Result<Option<String>, String> {
    backend.call(Request::AgentStart(Box::new(request))).await
}

/// A reasoning effort the user picked for a model (src/lib/reasoning.ts) in
/// a chat: runs using that model follow it. Returns how many runs it reached.
#[tauri::command]
pub async fn agent_set_effort(
    backend: State<'_, DesktopConnection>,
    profile_id: String,
    options: Option<OpenAIChatOptions>,
    dialog_id: Option<String>,
) -> Result<usize, String> {
    let request = Request::AgentSetEffort {
        profile_id,
        options,
        dialog_id,
    };
    backend.call(request).await
}

/// Stop: ends the chat's run as soon as it can (its request is cancelled;
/// what it waits on is answered "no").
#[tauri::command]
pub async fn agent_stop(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
) -> Result<bool, String> {
    let request = Request::AgentStop {
        dialog_id: dialog_id.clone(),
    };
    backend.call(request).await
}

/// A message for the chat's run while it works (steering-plan.md): it reads
/// it at its next step. A chat on a host sends its files first.
#[tauri::command]
pub async fn agent_steer(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    message: UserMessage,
) -> Result<SteerReply, String> {
    backend
        .call(Request::AgentSteer { dialog_id, message })
        .await
}

/// Takes back a message the chat's run hasn't read yet: whether it was
/// still waiting.
#[tauri::command]
pub async fn agent_unsteer(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    message_id: String,
) -> Result<bool, String> {
    backend
        .call(Request::AgentUnsteer {
            dialog_id,
            message_id,
        })
        .await
}

#[tauri::command]
pub async fn agent_resolve_approval(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    tool_call_id: String,
    decision: ApprovalDecision,
) -> Result<bool, String> {
    let request = Request::AgentResolveApproval {
        dialog_id: dialog_id.clone(),
        tool_call_id,
        decision,
    };
    backend.call(request).await
}

#[tauri::command]
pub async fn agent_answer_questions(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    tool_call_id: String,
    outcome: QuestionsOutcome,
) -> Result<bool, String> {
    let request = Request::AgentAnswerQuestions {
        dialog_id: dialog_id.clone(),
        tool_call_id,
        outcome,
    };
    backend.call(request).await
}

/// What's going on right now, here and on the connected hosts, for a
/// frontend that (re)starts mid-run.
#[tauri::command]
pub async fn agent_status(backend: State<'_, DesktopConnection>) -> Result<AgentStatus, String> {
    backend.call(Request::AgentStatus).await
}

/// The chat's current checklist, as the backend computes it.
#[tauri::command]
pub async fn chat_checklist(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
) -> Result<Option<CurrentChecklist>, String> {
    backend.call(Request::ChatChecklist { dialog_id }).await
}

/// What the chat's agent left running (bash's `background`), oldest first,
/// where the chat runs: its row of background tasks, as it opens.
#[tauri::command]
pub async fn background_tasks(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
) -> Result<Vec<BackgroundTask>, String> {
    let request = Request::BackgroundTasks {
        dialog_id: dialog_id.clone(),
    };
    backend.call(request).await
}

/// A task's Stop in that row: it stops with everything it started, and the
/// agent hears so. Whether it was running.
#[tauri::command]
pub async fn background_stop(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    id: String,
) -> Result<bool, String> {
    let request = Request::BackgroundStop {
        dialog_id: dialog_id.clone(),
        id,
    };
    backend.call(request).await
}

/// The checklist panel's dismiss ✕. Returns the chat's checklist after it.
#[tauri::command]
pub async fn chat_dismiss_checklist(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    message_id: String,
) -> Result<Option<CurrentChecklist>, String> {
    let request = Request::ChatDismissChecklist {
        dialog_id: dialog_id.clone(),
        message_id,
    };
    backend.call(request).await
}

/// A request the frontend makes itself, outside any run (a chat's title),
/// streamed to `on_event`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn send_chat_message(
    backend: State<'_, DesktopConnection>,
    base_url: String,
    api_key: String,
    model: String,
    messages: Option<Vec<WireChatMessage>>,
    dialog_id: Option<String>,
    context_tokens: Option<usize>,
    vision: Option<bool>,
    pdf: Option<bool>,
    current_user_content: Option<String>,
    streaming: bool,
    // Lets `cancel_chat_request` stop this request (and its retries).
    request_id: Option<String>,
    first_byte_timeout_ms: Option<u64>,
    continue_reply: Option<bool>,
    assistant_message_id: Option<String>,
    tools: Option<Vec<serde_json::Value>>,
    system_prompt: Option<String>,
    project_context: Option<String>,
    options: Option<OpenAIChatOptions>,
    context_mode: Option<String>,
    api_route: Option<String>,
    chat_id: Option<String>,
    on_event: Channel<serde_json::Value>,
) -> Result<serde_json::Value, serde_json::Value> {
    let params = Box::new(CompletionArgs {
        base_url,
        api_key,
        model,
        messages,
        dialog_id,
        context_tokens,
        vision,
        pdf,
        current_user_content,
        streaming,
        first_byte_timeout_ms,
        continue_reply,
        assistant_message_id,
        tools,
        system_prompt,
        project_context,
        options,
        context_mode,
        api_route,
        chat_id,
    });
    backend
        .desktop_task(
            |sink| {
                AgentRequest::Send {
                    request_id,
                    params,
                    sink,
                }
                .into()
            },
            move |event| on_event.send(event).map_err(|e| e.to_string()),
        )
        .await
}

/// Async so it runs off the main thread: Stop must work even if the main
/// thread is busy.
#[tauri::command]
pub async fn cancel_chat_request(
    backend: State<'_, DesktopConnection>,
    request_id: String,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(AgentRequest::Cancel { request_id }.into())
        .await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn chat_context_estimate(
    backend: State<'_, DesktopConnection>,
    dialog_id: String,
    base_url: String,
    model: String,
    context_tokens: Option<usize>,
    reasoning_effort: Option<ReasoningEffort>,
    vision: bool,
    pdf: Option<bool>,
    no_tools: Option<bool>,
    root: Option<String>,
    mode: Option<String>,
    extra_chars: Option<usize>,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(
            AgentRequest::Estimate {
                args: Box::new(EstimateArgs {
                    dialog_id,
                    base_url,
                    model,
                    context_tokens,
                    reasoning_effort,
                    vision,
                    pdf,
                    no_tools,
                    root,
                    mode,
                    extra_chars,
                }),
            }
            .into(),
        )
        .await
}

/// "Test & refresh models" on a connector (Settings → Connectors): whether
/// its program runs, its version and its models, here or on `host`, the
/// computer the chats using it would run it on.
#[tauri::command]
pub async fn connector_probe(
    backend: State<'_, DesktopConnection>,
    args: ProbeRequestArgs,
    host: Option<String>,
) -> Result<ConnectorProbe, String> {
    backend
        .call_on(host.as_deref(), Request::ConnectorProbe(args))
        .await
}

/// A connector's "Set up" step (install, sign in, sign out), here or on
/// `host`: started, followed, given the user's line, or stopped
/// (`SetupOp`). A step on a host runs as the user's own daemon there.
#[tauri::command]
pub async fn connector_setup(
    backend: State<'_, DesktopConnection>,
    op: SetupOp,
    host: Option<String>,
) -> Result<SetupJob, String> {
    backend
        .call_on(host.as_deref(), Request::ConnectorSetup(op))
        .await
}

/// The commands a connector's program offers in the chat's project folder
/// (`cwd`), here or on `host`: what the composer's `/` picker lists.
#[tauri::command]
pub async fn connector_commands(
    backend: State<'_, DesktopConnection>,
    args: ProbeRequestArgs,
    cwd: Option<String>,
    host: Option<String>,
) -> Result<Vec<ConnectorCommand>, String> {
    let request = Request::ConnectorCommands { args, cwd };
    backend.call_on(host.as_deref(), request).await
}

/// `GET {base_url}/models` on an OpenAI-compatible server.
#[tauri::command]
pub async fn list_models(
    backend: State<'_, DesktopConnection>,
    base_url: String,
    api_key: Option<String>,
) -> Result<serde_json::Value, String> {
    backend
        .desktop(AgentRequest::ListModels { base_url, api_key }.into())
        .await
}

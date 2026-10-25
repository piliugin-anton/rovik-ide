//! The agent's runs (agent_run), and the requests the frontend makes itself
//! (agent.rs). A chat on a host runs there (6.1).

use super::{answer, call, route};
use rovik_client::Remote;
use rovik_core::agent::{
    self, ChatParams, ChatStreamEvent, ContextEstimate, OpenAIChatOptions, ReasoningEffort,
    RemoteModel, WireChatMessage,
};
use rovik_core::agent_run::plan::{CurrentChecklist, QuestionsOutcome};
use rovik_core::agent_run::{
    AgentConfig, AgentStatus, ApprovalDecision, StartRequest, SteerReply, UserMessage,
};
use rovik_core::chat_request::ChatRequests;
use rovik_core::Core;
use rovik_protocol::connectors::{
    ConnectorCommand, ConnectorProbe, ProbeRequestArgs, SetupJob, SetupOp,
};
use rovik_protocol::events::BackgroundTask;
use rovik_protocol::Request;
use std::sync::Arc;
use tauri::ipc::Channel;
use tauri::State;

/// Here and on every connected host (6.1).
#[tauri::command]
pub async fn agent_configure(
    remote: State<'_, Arc<Remote>>,
    config: AgentConfig,
) -> Result<(), String> {
    answer(
        remote
            .everywhere(Request::AgentConfigure(Box::new(config)))
            .await,
    )
}

/// Starts a run in the chat. Returns its id, or None when there was nothing
/// to do (a Resume with nothing left to run).
#[tauri::command]
pub async fn agent_start(
    remote: State<'_, Arc<Remote>>,
    request: StartRequest,
) -> Result<Option<String>, String> {
    answer(remote.start(request).await)
}

/// A reasoning effort the user picked for a model (src/lib/reasoning.ts) in
/// a chat: runs using that model follow it. Returns how many runs it reached.
#[tauri::command]
pub async fn agent_set_effort(
    remote: State<'_, Arc<Remote>>,
    profile_id: String,
    options: Option<OpenAIChatOptions>,
    dialog_id: Option<String>,
) -> Result<usize, String> {
    let request = Request::AgentSetEffort {
        profile_id,
        options,
        dialog_id,
    };
    answer(remote.everywhere(request).await)
}

/// Stop: ends the chat's run as soon as it can (its request is cancelled;
/// what it waits on is answered "no").
#[tauri::command]
pub async fn agent_stop(remote: State<'_, Arc<Remote>>, dialog_id: String) -> Result<bool, String> {
    let request = Request::AgentStop {
        dialog_id: dialog_id.clone(),
    };
    route(&remote, &dialog_id, request).await
}

/// A message for the chat's run while it works (steering-plan.md): it reads
/// it at its next step. A chat on a host sends its files first.
#[tauri::command]
pub async fn agent_steer(
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    message: UserMessage,
) -> Result<SteerReply, String> {
    answer(remote.steer(&dialog_id, message).await)
}

/// Takes back a message the chat's run hasn't read yet: whether it was
/// still waiting.
#[tauri::command]
pub async fn agent_unsteer(
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    message_id: String,
) -> Result<bool, String> {
    answer(remote.unsteer(&dialog_id, message_id).await)
}

#[tauri::command]
pub async fn agent_resolve_approval(
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    tool_call_id: String,
    decision: ApprovalDecision,
) -> Result<bool, String> {
    let request = Request::AgentResolveApproval {
        dialog_id: dialog_id.clone(),
        tool_call_id,
        decision,
    };
    route(&remote, &dialog_id, request).await
}

#[tauri::command]
pub async fn agent_answer_questions(
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    tool_call_id: String,
    outcome: QuestionsOutcome,
) -> Result<bool, String> {
    let request = Request::AgentAnswerQuestions {
        dialog_id: dialog_id.clone(),
        tool_call_id,
        outcome,
    };
    route(&remote, &dialog_id, request).await
}

/// What's going on right now, here and on the connected hosts, for a
/// frontend that (re)starts mid-run.
#[tauri::command]
pub async fn agent_status(remote: State<'_, Arc<Remote>>) -> Result<AgentStatus, String> {
    remote.status().await.map_err(|fail| fail.message)
}

/// The chat's current checklist, as the backend computes it.
#[tauri::command]
pub async fn chat_checklist(
    core: State<'_, Core>,
    dialog_id: String,
) -> Result<Option<CurrentChecklist>, String> {
    call(&core, Request::ChatChecklist { dialog_id }).await
}

/// What the chat's agent left running (bash's `background`), oldest first,
/// where the chat runs: its row of background tasks, as it opens.
#[tauri::command]
pub async fn background_tasks(
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
) -> Result<Vec<BackgroundTask>, String> {
    let request = Request::BackgroundTasks {
        dialog_id: dialog_id.clone(),
    };
    route(&remote, &dialog_id, request).await
}

/// A task's Stop in that row: it stops with everything it started, and the
/// agent hears so. Whether it was running.
#[tauri::command]
pub async fn background_stop(
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    id: String,
) -> Result<bool, String> {
    let request = Request::BackgroundStop {
        dialog_id: dialog_id.clone(),
        id,
    };
    route(&remote, &dialog_id, request).await
}

/// The checklist panel's dismiss ✕. Returns the chat's checklist after it.
#[tauri::command]
pub async fn chat_dismiss_checklist(
    remote: State<'_, Arc<Remote>>,
    dialog_id: String,
    message_id: String,
) -> Result<Option<CurrentChecklist>, String> {
    let request = Request::ChatDismissChecklist {
        dialog_id: dialog_id.clone(),
        message_id,
    };
    route(&remote, &dialog_id, request).await
}

/// A request the frontend makes itself, outside any run (a chat's title),
/// streamed to `on_event`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn send_chat_message(
    core: State<'_, Core>,
    requests: State<'_, ChatRequests>,
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
    on_event: Channel<ChatStreamEvent>,
) -> Result<(), String> {
    let params = ChatParams {
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
        ..Default::default()
    };
    agent::send_chat_message(&core, &requests, request_id, params, move |event| {
        let _ = on_event.send(event);
    })
    .await
}

/// Async so it runs off the main thread: Stop must work even if the main
/// thread is busy.
#[tauri::command]
pub async fn cancel_chat_request(
    requests: State<'_, ChatRequests>,
    request_id: String,
) -> Result<bool, String> {
    Ok(requests.cancel(&request_id))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn chat_context_estimate(
    core: State<'_, Core>,
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
) -> Result<ContextEstimate, String> {
    agent::chat_context_estimate(
        &core,
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
    )
    .await
}

/// "Test & refresh models" on a connector (Settings → Connectors): whether
/// its program runs, its version and its models, here or on `host`, the
/// computer the chats using it would run it on.
#[tauri::command]
pub async fn connector_probe(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    args: ProbeRequestArgs,
    host: Option<String>,
) -> Result<ConnectorProbe, String> {
    match host.filter(|h| !h.is_empty()) {
        Some(host) => answer(remote.host_call(&host, Request::ConnectorProbe(args)).await),
        None => call(&core, Request::ConnectorProbe(args)).await,
    }
}

/// A connector's "Set up" step (install, sign in, sign out), here or on
/// `host`: started, followed, given the user's line, or stopped
/// (`SetupOp`). A step on a host runs as the user's own daemon there.
#[tauri::command]
pub async fn connector_setup(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    op: SetupOp,
    host: Option<String>,
) -> Result<SetupJob, String> {
    match host.filter(|h| !h.is_empty()) {
        Some(host) => answer(remote.host_call(&host, Request::ConnectorSetup(op)).await),
        None => call(&core, Request::ConnectorSetup(op)).await,
    }
}

/// The commands a connector's program offers in the chat's project folder
/// (`cwd`), here or on `host`: what the composer's `/` picker lists.
#[tauri::command]
pub async fn connector_commands(
    core: State<'_, Core>,
    remote: State<'_, Arc<Remote>>,
    args: ProbeRequestArgs,
    cwd: Option<String>,
    host: Option<String>,
) -> Result<Vec<ConnectorCommand>, String> {
    let request = Request::ConnectorCommands { args, cwd };
    match host.filter(|h| !h.is_empty()) {
        Some(host) => answer(remote.host_call(&host, request).await),
        None => call(&core, request).await,
    }
}

/// `GET {base_url}/models` on an OpenAI-compatible server.
#[tauri::command]
pub async fn list_models(
    base_url: String,
    api_key: Option<String>,
) -> Result<Vec<RemoteModel>, String> {
    agent::list_models(base_url, api_key).await
}

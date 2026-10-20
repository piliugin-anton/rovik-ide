//! The agent's runs (agent_run), and the requests the frontend makes itself
//! (agent.rs). A chat on a host runs there (6.1).

use super::{answer, call, route};
use rovik_client::Remote;
use rovik_core::agent::{
    self, ChatParams, ChatStreamEvent, ContextEstimate, OpenAIChatOptions, ReasoningEffort,
    RemoteModel, WireChatMessage,
};
use rovik_core::agent_run::plan::{CurrentChecklist, QuestionsOutcome};
use rovik_core::agent_run::{AgentConfig, AgentStatus, ApprovalDecision, StartRequest};
use rovik_core::chat_request::ChatRequests;
use rovik_core::Core;
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

/// `GET {base_url}/models` on an OpenAI-compatible server.
#[tauri::command]
pub async fn list_models(
    base_url: String,
    api_key: Option<String>,
) -> Result<Vec<RemoteModel>, String> {
    agent::list_models(base_url, api_key).await
}

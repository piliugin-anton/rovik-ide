//! The agent's runs (agent_run), and the requests the frontend makes itself
//! (agent.rs).

use crate::agent::{
    self, ChatParams, ChatStreamEvent, ContextEstimate, OpenAIChatOptions, ReasoningEffort,
    RemoteModel, WireChatMessage,
};
use crate::agent_run::plan::{CurrentChecklist, QuestionsOutcome};
use crate::agent_run::{self, AgentConfig, AgentStatus, ApprovalDecision, StartRequest};
use crate::chat_request::ChatRequests;
use crate::core::Core;
use tauri::ipc::Channel;
use tauri::State;

#[tauri::command]
pub fn agent_configure(core: State<'_, Core>, config: AgentConfig) {
    agent_run::agent_configure(&core, config);
}

/// Starts a run in the chat. Returns its id, or None when there was nothing
/// to do (a Resume with nothing left to run).
#[tauri::command]
pub async fn agent_start(
    core: State<'_, Core>,
    request: StartRequest,
) -> Result<Option<String>, String> {
    agent_run::agent_start(&core, request).await
}

/// A reasoning effort the user picked for a model (src/lib/reasoning.ts) in
/// a chat: runs using that model follow it. Returns how many runs it reached.
#[tauri::command]
pub fn agent_set_effort(
    core: State<'_, Core>,
    profile_id: String,
    options: Option<OpenAIChatOptions>,
    dialog_id: Option<String>,
) -> usize {
    agent_run::agent_set_effort(&core, profile_id, options, dialog_id)
}

/// Stop: ends the chat's run as soon as it can (its request is cancelled;
/// what it waits on is answered "no").
#[tauri::command]
pub fn agent_stop(core: State<'_, Core>, dialog_id: String) -> bool {
    agent_run::agent_stop(&core, dialog_id)
}

#[tauri::command]
pub fn agent_resolve_approval(
    core: State<'_, Core>,
    dialog_id: String,
    tool_call_id: String,
    decision: ApprovalDecision,
) -> bool {
    agent_run::agent_resolve_approval(&core, dialog_id, tool_call_id, decision)
}

#[tauri::command]
pub fn agent_answer_questions(
    core: State<'_, Core>,
    dialog_id: String,
    tool_call_id: String,
    outcome: QuestionsOutcome,
) -> bool {
    agent_run::agent_answer_questions(&core, dialog_id, tool_call_id, outcome)
}

/// What's going on right now, for a frontend that (re)starts mid-run.
#[tauri::command]
pub fn agent_status(core: State<'_, Core>) -> AgentStatus {
    agent_run::agent_status(&core)
}

/// The chat's current checklist, as the backend computes it.
#[tauri::command]
pub async fn chat_checklist(
    core: State<'_, Core>,
    dialog_id: String,
) -> Result<Option<CurrentChecklist>, String> {
    agent_run::chat_checklist(&core, dialog_id).await
}

/// The checklist panel's dismiss ✕. Returns the chat's checklist after it.
#[tauri::command]
pub async fn chat_dismiss_checklist(
    core: State<'_, Core>,
    dialog_id: String,
    message_id: String,
) -> Result<Option<CurrentChecklist>, String> {
    agent_run::chat_dismiss_checklist(&core, dialog_id, message_id).await
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

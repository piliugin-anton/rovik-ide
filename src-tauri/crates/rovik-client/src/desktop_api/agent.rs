//! One-off model requests and estimates, owned by the backend and scoped to a client.
use super::{encode_result, DesktopRequest, DesktopResult};
use crate::{desktop_backend::DesktopBackend, desktop_session::Session};
use rovik_core::agent::{self, ChatParams, OpenAIChatOptions, ReasoningEffort, WireChatMessage};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Serializable subset of a frontend completion request; run-only callbacks stay in Core.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CompletionArgs {
    /// The frontend completion parameter `base_url`.
    pub base_url: String,
    /// The frontend completion parameter `api_key`.
    pub api_key: String,
    /// The frontend completion parameter `model`.
    pub model: String,
    /// The frontend completion parameter `messages`.
    pub messages: Option<Vec<WireChatMessage>>,
    /// The frontend completion parameter `dialog_id`.
    pub dialog_id: Option<String>,
    /// The frontend completion parameter `context_tokens`.
    pub context_tokens: Option<usize>,
    /// The frontend completion parameter `vision`.
    pub vision: Option<bool>,
    /// The frontend completion parameter `pdf`.
    pub pdf: Option<bool>,
    /// The frontend completion parameter `current_user_content`.
    pub current_user_content: Option<String>,
    /// The frontend completion parameter `streaming`.
    pub streaming: bool,
    /// The frontend completion parameter `first_byte_timeout_ms`.
    pub first_byte_timeout_ms: Option<u64>,
    /// The frontend completion parameter `continue_reply`.
    pub continue_reply: Option<bool>,
    /// The frontend completion parameter `assistant_message_id`.
    pub assistant_message_id: Option<String>,
    /// The frontend completion parameter `tools`.
    pub tools: Option<Vec<serde_json::Value>>,
    /// The frontend completion parameter `system_prompt`.
    pub system_prompt: Option<String>,
    /// The frontend completion parameter `project_context`.
    pub project_context: Option<String>,
    /// The frontend completion parameter `options`.
    pub options: Option<OpenAIChatOptions>,
    /// The frontend completion parameter `context_mode`.
    pub context_mode: Option<String>,
    /// The frontend completion parameter `api_route`.
    pub api_route: Option<String>,
    /// The frontend completion parameter `chat_id`.
    pub chat_id: Option<String>,
}

impl From<CompletionArgs> for ChatParams {
    fn from(args: CompletionArgs) -> Self {
        Self {
            base_url: args.base_url,
            api_key: args.api_key,
            model: args.model,
            messages: args.messages,
            dialog_id: args.dialog_id,
            context_tokens: args.context_tokens,
            vision: args.vision,
            pdf: args.pdf,
            current_user_content: args.current_user_content,
            streaming: args.streaming,
            first_byte_timeout_ms: args.first_byte_timeout_ms,
            continue_reply: args.continue_reply,
            assistant_message_id: args.assistant_message_id,
            tools: args.tools,
            system_prompt: args.system_prompt,
            project_context: args.project_context,
            options: args.options,
            context_mode: args.context_mode,
            api_route: args.api_route,
            chat_id: args.chat_id,
            ..Default::default()
        }
    }
}

/// Parameters used to estimate a stored chat's context.
#[derive(Serialize, Deserialize)]
pub struct EstimateArgs {
    /// The context estimate parameter `dialog_id`.
    pub dialog_id: String,
    /// The context estimate parameter `base_url`.
    pub base_url: String,
    /// The context estimate parameter `model`.
    pub model: String,
    /// The context estimate parameter `context_tokens`.
    pub context_tokens: Option<usize>,
    /// The context estimate parameter `reasoning_effort`.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// The context estimate parameter `vision`.
    pub vision: bool,
    /// The context estimate parameter `pdf`.
    pub pdf: Option<bool>,
    /// The context estimate parameter `no_tools`.
    pub no_tools: Option<bool>,
    /// The context estimate parameter `root`.
    pub root: Option<String>,
    /// The context estimate parameter `mode`.
    pub mode: Option<String>,
    /// The context estimate parameter `extra_chars`.
    pub extra_chars: Option<usize>,
}

/// Frontend model operations keep the existing argument and event shapes.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum AgentRequest {
    /// Starts a finite completion with a private progress stream.
    Send {
        request_id: Option<String>,
        params: Box<CompletionArgs>,
        sink: String,
    },
    /// Cancels only this client's one-off request, including an early Stop.
    Cancel { request_id: String },
    /// Estimates a chat's context through the backend's database and services.
    Estimate { args: Box<EstimateArgs> },
    /// Discovers models using the backend's network settings.
    ListModels {
        base_url: String,
        api_key: Option<String>,
    },
}

impl AgentRequest {
    pub(super) async fn dispatch(
        self,
        backend: &DesktopBackend,
        session: Option<&Arc<Session>>,
    ) -> DesktopResult {
        let owner = session.ok_or_else(|| {
            serde_json::Value::String("A client session is required for model requests".into())
        })?;
        match self {
            Self::Send {
                request_id,
                params,
                sink,
            } => {
                let core = backend.core.clone();
                let requests = owner.chat_requests.clone();
                encode_result(crate::desktop_task::start(
                    owner,
                    sink,
                    move |output| async move {
                        encode_result(
                            agent::send_chat_message(
                                &core,
                                &requests,
                                request_id,
                                (*params).into(),
                                move |event| {
                                    output.emit(event);
                                },
                            )
                            .await,
                        )
                    },
                ))
            }
            Self::Cancel { request_id } => {
                encode_result(Ok::<_, String>(owner.chat_requests.cancel(&request_id)))
            }
            Self::ListModels { base_url, api_key } => {
                encode_result(agent::list_models(base_url, api_key).await)
            }
            Self::Estimate { args } => encode_result(
                agent::chat_context_estimate(
                    &backend.core,
                    args.dialog_id,
                    args.base_url,
                    args.model,
                    args.context_tokens,
                    args.reasoning_effort,
                    args.vision,
                    args.pdf,
                    args.no_tools,
                    args.root,
                    args.mode,
                    args.extra_chars,
                )
                .await,
            ),
        }
    }
}

impl From<AgentRequest> for DesktopRequest {
    fn from(request: AgentRequest) -> Self {
        Self::Agent(Box::new(request))
    }
}

//! A chat's rows as they travel (the core's chat_db/store.rs).

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
    /// Provider data that must be sent back unchanged with this call, e.g.
    /// Gemini 3's `{"google": {"thought_signature": "…"}}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_content: Option<Value>,
}

/// Changes to a stored message. Absent fields stay as they are.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MessagePatch {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub meta: Option<Value>,
    pub error: Option<String>,
    pub failed: Option<bool>,
    #[serde(default)]
    pub finalize: bool,
}

/// The model a chat last used, and its effort (lib/reasoning.ts).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatModel {
    pub provider_id: String,
    pub model_id: String,
    pub effort: Option<String>,
}

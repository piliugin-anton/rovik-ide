//! The agent's questions and its checklist (the core's agent_run/plan.rs).

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recommended: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub options: Vec<QuestionOption>,
    /// "Other" with the user's own words.
    pub allow_custom: bool,
}

/// One question's answer.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum QuestionAnswer {
    Option {
        label: String,
    },
    Custom {
        text: String,
    },
    /// "You decide".
    Agent,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum QuestionsOutcome {
    Answers {
        answers: Vec<QuestionAnswer>,
    },
    /// Typed in the composer instead of answering in the card.
    Reply {
        reply: String,
    },
    Stopped {
        stopped: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodoItem {
    pub title: String,
    pub status: String,
}

/// The chat's current checklist (`plan::current_checklist` in the core):
/// its items, whether it was carried from an earlier request, and the
/// message whose `todo` call holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CurrentChecklist {
    pub items: Vec<TodoItem>,
    pub carried: bool,
    pub from_message_id: Option<String>,
}

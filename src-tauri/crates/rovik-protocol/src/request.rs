//! The calls a client makes (4.2), and their replies. A request's fields
//! are the arguments of the Tauri command of the same name, so the app's
//! own calls and the daemon's are one list of operations (D3).

use crate::agent::{AgentConfig, AgentStatus, ApprovalDecision, OpenAIChatOptions, StartRequest};
use crate::chat::{ChatModel, MessagePatch};
use crate::clm::{ClmStatus, RevisionRow};
use crate::host::HostInfo;
use crate::plan::{CurrentChecklist, QuestionsOutcome};
use crate::work::{Project, Synced};
use crate::{Fail, FailCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Request {
    // Agent: runs, and what they wait on.
    AgentStart(StartRequest),
    AgentStop {
        dialog_id: String,
    },
    AgentResolveApproval {
        dialog_id: String,
        tool_call_id: String,
        decision: ApprovalDecision,
    },
    AgentAnswerQuestions {
        dialog_id: String,
        tool_call_id: String,
        outcome: QuestionsOutcome,
    },
    AgentStatus,
    AgentConfigure(AgentConfig),
    AgentSetEffort {
        profile_id: String,
        options: Option<OpenAIChatOptions>,
        dialog_id: Option<String>,
    },
    ChatChecklist {
        dialog_id: String,
    },
    ChatDismissChecklist {
        dialog_id: String,
        message_id: String,
    },
    WorkloadFocus {
        dialog_id: Option<String>,
    },

    // Chat: writes to a chat's rows.
    ChatRename {
        dialog_id: String,
        title: String,
    },
    ChatUpdateMessage {
        message_id: String,
        patch: MessagePatch,
    },
    ChatSetModel {
        dialog_id: String,
        model: ChatModel,
    },
    ChatDelete {
        dialog_id: String,
    },
    /// Of these attachments, those the daemon has no file for, a row alone
    /// not counting (K47). The others' grace period starts again, so its
    /// cleanup keeps them until the message that names them is stored
    /// (6.11). → `Ids`.
    BlobsMissing {
        ids: Vec<String>,
    },

    // CLM: a chat's context file and its revisions.
    ClmStatus {
        dialog_id: String,
    },
    ClmSetMode {
        dialog_id: String,
        mode: String,
    },
    ClmReset {
        dialog_id: String,
    },
    ClmListRevisions {
        dialog_id: String,
    },
    ClmReadRevision {
        dialog_id: String,
        number: u64,
    },
    ClmRestoreRevision {
        dialog_id: String,
        number: u64,
    },

    // Work: the project on the server (6.4–6.7).
    /// Creates the project's repository and row on the daemon, or brings
    /// them up to date: its name and settings, and the ignore rules only
    /// the client has (K32). → `Done`.
    ProjectEnsure {
        project: Project,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        settings: Option<Value>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        excludes: Vec<String>,
    },
    /// Brings the chat's worktree to a snapshot pushed before it (6.7). A
    /// chat without one gets it there, on its own branch (6.4).
    /// → `Synced`.
    WorkSync {
        dialog_id: String,
        project: String,
        snapshot: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        excludes: Vec<String>,
    },

    // Host: the server, for the hosts page, deploy and upgrade.
    HostInfo,
}

/// A request's reply, by what it carries. A caller takes the value its
/// request answers with through `TryFrom`: `bool::try_from(response)`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "t", content = "v", rename_all = "camelCase")]
pub enum Response {
    /// Done, with nothing to tell.
    Done,
    Bool(bool),
    /// `AgentStart`: the run's id, or none when there was nothing to do.
    RunId(Option<String>),
    /// `AgentSetEffort`: how many runs it reached.
    Count(usize),
    AgentStatus(AgentStatus),
    Checklist(Option<CurrentChecklist>),
    ClmStatus(ClmStatus),
    Revisions(Vec<RevisionRow>),
    Text(String),
    HostInfo(HostInfo),
    /// `BlobsMissing`: the attachments it lacks.
    Ids(Vec<String>),
    /// `WorkSync`.
    Synced(Synced),
}

/// A reply of another kind than its request's is the other side's fault.
fn unexpected() -> Fail {
    Fail::new(FailCode::Internal, "The reply doesn't answer the request.")
}

impl TryFrom<Response> for () {
    type Error = Fail;

    fn try_from(response: Response) -> Result<(), Fail> {
        match response {
            Response::Done => Ok(()),
            _ => Err(unexpected()),
        }
    }
}

macro_rules! replies {
    ($($variant:ident => $value:ty),* $(,)?) => {$(
        impl TryFrom<Response> for $value {
            type Error = Fail;

            fn try_from(response: Response) -> Result<$value, Fail> {
                match response {
                    Response::$variant(value) => Ok(value),
                    _ => Err(unexpected()),
                }
            }
        }
    )*};
}

replies! {
    Bool => bool,
    RunId => Option<String>,
    Count => usize,
    AgentStatus => AgentStatus,
    Checklist => Option<CurrentChecklist>,
    ClmStatus => ClmStatus,
    Revisions => Vec<RevisionRow>,
    Text => String,
    HostInfo => HostInfo,
    Ids => Vec<String>,
    Synced => Synced,
}

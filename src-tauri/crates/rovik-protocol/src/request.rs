//! The calls a client makes (4.2), and their replies. A request's fields
//! are the arguments of the Tauri command of the same name, so the app's
//! own calls and the daemon's are one list of operations (D3).

use crate::agent::{AgentConfig, AgentStatus, ApprovalDecision, OpenAIChatOptions, StartRequest};
use crate::chat::{ChatModel, MessagePatch};
use crate::clm::{ClmStatus, RevisionRow};
use crate::host::HostInfo;
use crate::plan::{CurrentChecklist, QuestionsOutcome};
use crate::work::{ChatResult, Handed, Project, Synced, UndoneMerge, WorkChange, WorkFile};
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
    /// chat without one gets it there, on its own branch (6.4), with
    /// `changes`, a commit on the snapshot, written into it: a chat that
    /// moved with its overlay (T09). → `Synced`.
    WorkSync {
        dialog_id: String,
        project: String,
        snapshot: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        excludes: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        changes: Option<String>,
    },
    /// Commits the chat's worktree as its result now, as a run's end does
    /// (6.5). → `ChatResult`.
    WorkResult {
        dialog_id: String,
    },
    /// The newest result of each of this computer's chats, for the ones a
    /// client missed (K24). → `ChatResults`.
    WorkResults,
    /// The chat's result would land merged with the project's newer
    /// changes (6.6, D13): `snapshot` is merged into its worktree as a sync
    /// does, and the chat starts with the checks on what merged
    /// (`CheckMerged`), whose end brings a new result. → `Synced`.
    WorkRecheck {
        dialog_id: String,
        project: String,
        snapshot: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        excludes: Vec<String>,
    },
    /// What the chat changed in its worktree so far, from its landing base
    /// (6.1). → `Changes`.
    WorkStatus {
        dialog_id: String,
    },
    /// A file of the chat's worktree, from the project's folder: as its
    /// landing base has it and as the chat has it now (6.1). → `WorkFile`.
    WorkFile {
        dialog_id: String,
        path: String,
    },
    /// Throws the chat's changes away: its worktree back at its landing
    /// base (6.1). → `Done`.
    WorkDiscard {
        dialog_id: String,
    },
    /// Hands conflicts to the chat (6.1): markers in `files`, from the
    /// project's folder, between the chat's version and the project's in
    /// `snapshot`, pushed before it, which those files' landing base takes.
    /// → `Handed`.
    WorkConflictsToChat {
        dialog_id: String,
        snapshot: String,
        files: Vec<String>,
    },
    /// Takes the project's merged changes and conflict markers back out of
    /// the chat's files (6.1): each is the chat's own version again, on its
    /// landing base from before. `files`, from the project's folder: only
    /// those (Unmark). A file changed since stays as it is. The chat's next
    /// result is committed at once. → `UndoneMerge`.
    WorkUndoMerge {
        dialog_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        files: Option<Vec<String>>,
    },
    /// A result of the chat landed on its computer (6.5): for these paths,
    /// from the root of the project's repository, the chat's landing base
    /// takes the result's content, so the next result doesn't bring back a
    /// landing the user undid (K34). → `Done`.
    WorkLanded {
        dialog_id: String,
        commit: String,
        paths: Vec<String>,
    },

    // Host: the server, for the hosts page, deploy and upgrade.
    HostInfo,
}

/// The chat a request is about: by its id, or by one of its messages'.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target<'a> {
    Chat(&'a str),
    Message(&'a str),
}

/// What a request does to its chat, for the refusal of a chat owned
/// elsewhere (4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Reads it: allowed wherever its rows are.
    Read,
    /// Controls the run going for it: stops it or answers what it waits
    /// on. Allowed for a chat that is moving, whose run pauses (6.8).
    Run,
    /// Starts a run or changes its rows.
    Write,
}

impl Request {
    /// The chat the request is about, and what it does to it. None: about
    /// no chat, or about runs in memory only (`AgentSetEffort`,
    /// `WorkloadFocus`), which change no chat's rows.
    pub fn target(&self) -> Option<(Target<'_>, Access)> {
        use Access::{Read, Run, Write};
        fn chat(id: &str, access: Access) -> Option<(Target<'_>, Access)> {
            Some((Target::Chat(id), access))
        }
        match self {
            Request::AgentStart(start) => chat(&start.dialog_id, Write),
            Request::AgentStop { dialog_id }
            | Request::AgentResolveApproval { dialog_id, .. }
            | Request::AgentAnswerQuestions { dialog_id, .. } => chat(dialog_id, Run),
            Request::ChatChecklist { dialog_id }
            | Request::ClmStatus { dialog_id }
            | Request::ClmListRevisions { dialog_id }
            | Request::ClmReadRevision { dialog_id, .. }
            | Request::WorkStatus { dialog_id }
            | Request::WorkFile { dialog_id, .. } => chat(dialog_id, Read),
            Request::ChatDismissChecklist { dialog_id, .. }
            | Request::ChatRename { dialog_id, .. }
            | Request::ChatSetModel { dialog_id, .. }
            | Request::ChatDelete { dialog_id }
            | Request::ClmSetMode { dialog_id, .. }
            | Request::ClmReset { dialog_id }
            | Request::ClmRestoreRevision { dialog_id, .. }
            | Request::WorkSync { dialog_id, .. }
            | Request::WorkResult { dialog_id }
            | Request::WorkRecheck { dialog_id, .. }
            | Request::WorkDiscard { dialog_id }
            | Request::WorkConflictsToChat { dialog_id, .. }
            | Request::WorkUndoMerge { dialog_id, .. }
            | Request::WorkLanded { dialog_id, .. } => chat(dialog_id, Write),
            Request::ChatUpdateMessage { message_id, .. } => {
                Some((Target::Message(message_id.as_str()), Write))
            }
            Request::AgentStatus
            | Request::AgentConfigure(_)
            | Request::AgentSetEffort { .. }
            | Request::WorkloadFocus { .. }
            | Request::BlobsMissing { .. }
            | Request::ProjectEnsure { .. }
            | Request::WorkResults
            | Request::HostInfo => None,
        }
    }
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
    /// `WorkResult`.
    ChatResult(ChatResult),
    /// `WorkResults`.
    ChatResults(Vec<ChatResult>),
    /// `WorkStatus`.
    Changes(Vec<WorkChange>),
    WorkFile(WorkFile),
    /// `WorkConflictsToChat`.
    Handed(Handed),
    /// `WorkUndoMerge`.
    UndoneMerge(UndoneMerge),
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
    ChatResult => ChatResult,
    ChatResults => Vec<ChatResult>,
    Changes => Vec<WorkChange>,
    WorkFile => WorkFile,
    Handed => Handed,
    UndoneMerge => UndoneMerge,
}

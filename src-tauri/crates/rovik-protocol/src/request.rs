//! The calls a client makes (4.2), and their replies. A request's fields
//! are the arguments of the Tauri command of the same name, so the app's
//! own calls and the daemon's are one list of operations (D3).

use crate::agent::{AgentConfig, AgentStatus, ApprovalDecision, OpenAIChatOptions, StartRequest};
use crate::autopilot::{AutonomyConfig, ChatTask, DeliveryTest};
use crate::chat::{ChatModel, MessagePatch};
use crate::clm::{ClmStatus, RevisionRow};
use crate::host::{HostInfo, KeyInfo, Storage};
use crate::plan::{CurrentChecklist, QuestionsOutcome};
use crate::sync::{ChangesCursor, ChangesPage, Ownership};
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
    /// Pauses the chat's run for its move to `host` (6.8), at the run's
    /// next safe point: once its tool calls have their results and its
    /// subagents are done. `host` is empty for a move back to the chat's
    /// computer. Answers when the run has ended: whether it paused for the
    /// move, which `ContinueRun` continues where the chat goes. False when
    /// no run went. → `Bool`.
    AgentPause {
        dialog_id: String,
        host: String,
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
    /// Beside `AgentConfigure`: what a server that works on its own keeps
    /// of this computer's settings for runs that go on without it (H02).
    /// Served with `features::AUTONOMY`.
    AutonomyConfigure(AutonomyConfig),
    /// "Test delivery" (H05): pushes an empty branch `rovik/test-<date>`
    /// to `remote` as a task's result would go, with this computer's
    /// deploy key, and deletes it. Served with `features::AUTONOMY`.
    DeliveryTest {
        remote: String,
    },
    /// This computer's chats on the server, each with its latest Autopilot
    /// task (H08), as `rovik-agentd status` lists them. Served with
    /// `features::AUTONOMY`.
    TaskStatus,
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
    /// The rows of the chat and its subagents' chats changed since the
    /// owner's revision `rev`, a page of at most 512 KB at a time (6.3):
    /// `cursor` is where the last page said to go on from. A chat its
    /// computer took back can still be read. → `ChatChanges`.
    ChatChangesSince {
        dialog_id: String,
        rev: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<ChangesCursor>,
    },
    /// The chat as the daemon has it for this computer, and the move that
    /// brought it there (6.8): what a move cut short asks. → `Ownership`,
    /// none when the daemon doesn't know the chat.
    ChatOwner {
        dialog_id: String,
    },
    /// Gives the chat back to its computer (6.8): the daemon refuses what
    /// would change it from then on, and keeps its rows for the computer's
    /// last pull until `ChatForget`. With `move_id`, only if that move
    /// brought it: a release queued when a move was cancelled offline never
    /// gives back a later move's chat. → `Bool`, whether it's released.
    ChatRelease {
        dialog_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        move_id: Option<String>,
    },
    /// Deletes the rows of a chat its computer took back (6.8), which has
    /// them all: its last pull came after the release. Refused for a chat
    /// the daemon hasn't released. → `Bool`, whether the daemon knew the
    /// chat.
    ChatForget {
        dialog_id: String,
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
        /// Where a server that pushes its results pushes them (D15, H04):
        /// the project's `origin` as an SSH address, without credentials
        /// (`autopilot::push_url`). Sent to such a server only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remote: Option<String>,
    },
    /// Brings the chat's worktree to a snapshot pushed before it (6.7). A
    /// chat without one gets it there, on its own branch (6.4), with
    /// `changes`, a commit on the snapshot, written into it: a chat that
    /// moved with its overlay (T09). With `mark`, the user handed the
    /// sync's text conflicts to the chat (U05): they get markers whatever
    /// "chat resolves conflicts" says, and the message that asks the chat
    /// to settle them tells it. → `Synced`.
    WorkSync {
        dialog_id: String,
        project: String,
        snapshot: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        excludes: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        changes: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        mark: bool,
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
    /// The providers whose key the host keeps, and the base URL each key
    /// goes with: never the keys. → `Keys`.
    KeysList,
    /// Forgets `provider`'s key, or every key; the next start that brings
    /// one keeps it again (D11). → `Count`, the keys forgotten.
    KeysRemove {
        provider: Option<String>,
    },
    /// What Rovik's folders take on the host. → `Storage`.
    Storage,
    /// Collects now what Rovik's folders no longer need, as each day does
    /// (R01): what's left of chats deleted or taken back, attachments no
    /// message names, and the repositories' garbage. → `Storage`, what the
    /// folders take after.
    CleanUp,
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
    /// Controls the run going for it: stops or pauses it, or answers what
    /// it waits on. Allowed for a chat that is moving, whose run pauses
    /// (6.8).
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
            | Request::AgentPause { dialog_id, .. }
            | Request::AgentResolveApproval { dialog_id, .. }
            | Request::AgentAnswerQuestions { dialog_id, .. } => chat(dialog_id, Run),
            Request::ChatChecklist { dialog_id }
            | Request::ChatChangesSince { dialog_id, .. }
            | Request::ChatOwner { dialog_id }
            | Request::ClmStatus { dialog_id }
            | Request::ClmListRevisions { dialog_id }
            | Request::ClmReadRevision { dialog_id, .. }
            | Request::WorkStatus { dialog_id }
            | Request::WorkFile { dialog_id, .. } => chat(dialog_id, Read),
            Request::ChatDismissChecklist { dialog_id, .. }
            | Request::ChatRename { dialog_id, .. }
            | Request::ChatSetModel { dialog_id, .. }
            | Request::ChatDelete { dialog_id }
            | Request::ChatRelease { dialog_id, .. }
            | Request::ChatForget { dialog_id }
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
            | Request::AutonomyConfigure(_)
            | Request::DeliveryTest { .. }
            | Request::TaskStatus
            | Request::AgentSetEffort { .. }
            | Request::WorkloadFocus { .. }
            | Request::BlobsMissing { .. }
            | Request::ProjectEnsure { .. }
            | Request::WorkResults
            | Request::HostInfo
            | Request::KeysList
            | Request::KeysRemove { .. }
            | Request::Storage
            | Request::CleanUp => None,
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
    /// `ChatChangesSince`.
    ChatChanges(ChangesPage),
    /// `ChatOwner`.
    Ownership(Option<Ownership>),
    /// `KeysList`.
    Keys(Vec<KeyInfo>),
    Storage(Storage),
    /// `DeliveryTest`.
    DeliveryTested(DeliveryTest),
    /// `TaskStatus`.
    Tasks(Vec<ChatTask>),
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
    ChatChanges => ChangesPage,
    Ownership => Option<Ownership>,
    Keys => Vec<KeyInfo>,
    Storage => Storage,
    DeliveryTested => DeliveryTest,
    Tasks => Vec<ChatTask>,
}

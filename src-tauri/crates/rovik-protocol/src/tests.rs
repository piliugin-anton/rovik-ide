//! Every variant of every protocol enum goes through JSON and comes back
//! the same, as the same JSON (plan rule 7). A variant without a sample
//! here fails `covers`, so a new request can't go untested.

use crate::agent::*;
use crate::chat::{ChatModel, MessagePatch, ToolCall};
use crate::clm::{ClmStatus, RevisionRow};
use crate::diff::Hunk;
use crate::events::*;
use crate::host::{HostInfo, Install, KeyInfo, Storage};
use crate::permissions::{ApprovalRule, Approver, ChecklistMode, Permissions};
use crate::plan::*;
use crate::subagents::{KindConfig, ModelChoice, SubagentConfig};
use crate::sync::*;
use crate::work::{
    Change, ChatResult, ChatSync, Handed, Project, SyncState, Synced, UndoneMerge, WorkChange,
    WorkFile,
};
use crate::*;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Debug;

/// A page with one of each row a chat has, as the owner stores them.
fn changes_page() -> ChangesPage {
    ChangesPage {
        changes: ChangeSet {
            dialogs: vec![StoredDialog {
                id: "d".into(),
                title: "Chat".into(),
                created_at: 1,
                updated_at: 2,
                message_count: 1,
                preview: Some("Hello".into()),
                archived: 0,
                parent_id: None,
                parent_call_id: None,
                kind: None,
                context_mode: Some("clm".into()),
                model_provider: Some("p".into()),
                model_id: Some("m".into()),
                effort: None,
            }],
            clm_states: vec![StoredClmState {
                dialog_id: "d".into(),
                tokens: 10,
                budget: 100,
                updated_at: 3,
                rendered: Some("# Context".into()),
                ..Default::default()
            }],
            clm_revisions: vec![StoredRevision {
                dialog_id: "d".into(),
                number: 4,
                kind: "context".into(),
                created_at: 3,
                tokens: 10,
                content: "# Context".into(),
            }],
            messages: vec![StoredMessage {
                id: "m".into(),
                dialog_id: "d".into(),
                seq: 1024,
                role: "tool".into(),
                body_external: true,
                body_bytes: 9000,
                tool_call_id: Some("c1".into()),
                meta: Some("{\"paused\":null}".into()),
                finalized: true,
                created_at: 4,
                content: "the preview".into(),
                body: Some("the whole result".into()),
                attachments: vec!["a".repeat(64)],
                extras: vec![StoredExtra {
                    call_id: "c1".into(),
                    extra_content: "{}".into(),
                    created_at: 4,
                }],
                ..Default::default()
            }],
            clm_kept: Some(BTreeMap::from([("d".to_string(), vec![4])])),
        },
        next: Some(ChangesCursor {
            upto: 57,
            chat: "d".into(),
            table: ChangedTable::Messages,
            rev: 50,
            key: "m".into(),
        }),
        rev: 57,
    }
}

fn result(pending: Option<&str>) -> ChatResult {
    ChatResult {
        dialog_id: "d".into(),
        commit: "0123456789abcdef0123456789abcdef01234567".into(),
        seq: 3,
        outcome: "done".into(),
        pending: pending.map(String::from),
        marked: Vec::new(),
        unchecked: false,
    }
}

/// Through JSON and back: the same value, and the same JSON again.
fn round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T) -> Value {
    let json = serde_json::to_value(value).unwrap();
    let back: T = serde_json::from_value(json.clone())
        .unwrap_or_else(|e| panic!("{json} doesn't read back: {e}"));
    assert_eq!(&back, value, "{json}");
    assert_eq!(serde_json::to_value(&back).unwrap(), json);
    json
}

/// The tags serde knows for `T`, from the error it gives for one it doesn't
/// know ("unknown variant `?`, expected one of `a`, `b`").
fn tags<T: DeserializeOwned + Debug>(tag: &str) -> BTreeSet<String> {
    let err = serde_json::from_value::<T>(json!({ tag: "?" }))
        .unwrap_err()
        .to_string();
    let list = err
        .split("expected one of ")
        .nth(1)
        .unwrap_or_else(|| panic!("no variants in: {err}"));
    list.split(", ")
        .map(|t| t.trim_matches(|c: char| c == '`' || !c.is_alphanumeric()))
        .map(String::from)
        .collect()
}

/// Every sample round-trips, and together they have every variant of `T`.
fn covers<T: Serialize + DeserializeOwned + PartialEq + Debug>(tag: &str, samples: &[T]) {
    let seen: BTreeSet<String> = samples
        .iter()
        .map(|s| round_trip(s)[tag].as_str().unwrap().to_string())
        .collect();
    assert_eq!(seen, tags::<T>(tag), "variants without a sample");
}

fn profile(id: &str) -> ModelProfile {
    ModelProfile {
        id: id.into(),
        label: "Test/m".into(),
        model: "m".into(),
        base_url: "https://example.test/v1".into(),
        api_key: "sk-test".into(),
        stream: true,
        vision: true,
        pdf: false,
        context_tokens: Some(128_000),
        no_tools: false,
        first_byte_timeout_ms: Some(30_000),
        api_route: Some("responses".into()),
    }
}

fn options() -> OpenAIChatOptions {
    OpenAIChatOptions {
        max_tokens: Some(4096),
        temperature: Some(0.5),
        top_p: None,
        stop: Some(vec!["END".into()]),
        reasoning_effort: Some(ReasoningEffort::High),
        reasoning_formats: Some(vec!["reasoning_effort".into()]),
    }
}

fn config() -> AgentConfig {
    let mut kinds = HashMap::new();
    kinds.insert(
        "scout".to_string(),
        KindConfig {
            enabled: true,
            models: vec![
                ModelChoice::Profile(profile("p::scout")),
                ModelChoice::Chat { chat: true },
            ],
            options: Some(options()),
            max_tool_calls: Some(20),
            max_minutes: Some(2.5),
            ..Default::default()
        },
    );
    AgentConfig {
        permissions: Permissions {
            allow_any_command: true,
            denied_commands: vec!["git push".into()],
            command_approval: ApprovalRule {
                required: true,
                by: Approver::Ai,
            },
            tool_call_checkpoint: None,
            checklist: ChecklistMode::Always,
            ..Default::default()
        },
        approval_profiles: vec![profile("p::reviewer")],
        subagents: SubagentConfig {
            enabled: true,
            kinds,
            max_at_once: Some(2),
            max_run_tokens: Some(1_000_000),
            parallel_workers: true,
            ..Default::default()
        },
    }
}

fn start() -> StartRequest {
    StartRequest {
        dialog_id: "d".into(),
        start: StartKind::Send {
            message: UserMessage {
                id: "u".into(),
                content: "Hello".into(),
                meta: Some(json!({ "pdfs": [{ "id": "f", "name": "a.pdf", "pages": 2 }] })),
                attachments: vec!["f".into()],
            },
        },
        profile: profile("p::m"),
        options: Some(options()),
        mode: Some("agent".into()),
        context_mode: Some("clm".into()),
        root: Some("/p".into()),
        current_user_content: Some("Hello, with context".into()),
        local_date: Some("2026-10-15".into()),
        config: Some(config()),
        isolate: Some(false),
        project_settings: Some(json!({ "checks": [{ "kind": "test", "command": "npm test" }] })),
    }
}

fn approval() -> ApprovalRequest {
    ApprovalRequest {
        tool_call_id: "c".into(),
        dialog_id: "d".into(),
        tool_name: "edit".into(),
        summary: "Edit a.ts".into(),
        command: None,
        note: Some("Asked by the reviewer".into()),
        edit: Some(EditProposal {
            abs_path: "/p/a.ts".into(),
            rel_path: "a.ts".into(),
            current: "a\n".into(),
            proposed: "b\n".into(),
        }),
        parent_dialog_id: Some("lead".into()),
        parent_call_id: Some("t1".into()),
        remote: true,
    }
}

fn questions() -> QuestionsRequest {
    QuestionsRequest {
        tool_call_id: "q".into(),
        dialog_id: "d".into(),
        questions: vec![Question {
            question: "Which?".into(),
            reason: None,
            options: vec![QuestionOption {
                label: "A".into(),
                description: Some("The first".into()),
                recommended: true,
            }],
            allow_custom: false,
        }],
    }
}

fn clm_status() -> ClmStatus {
    ClmStatus {
        mode: Some("clm".into()),
        available: false,
        problem: Some("Virtual files aren't mounted.".into()),
        path: "context.md".into(),
        real_path: None,
        revision: Some(3),
        revisions: 3,
        edits: 1,
        projected: false,
        tokens: 900,
        budget: 32_000,
        compliance: Some("a compaction is due".into()),
    }
}

fn checklist() -> CurrentChecklist {
    CurrentChecklist {
        items: vec![TodoItem {
            title: "Step".into(),
            status: "pending".into(),
        }],
        carried: true,
        from_message_id: Some("m".into()),
    }
}

#[test]
fn every_frame_round_trips() {
    covers(
        "t",
        &[
            Frame::Hello {
                protocol: PROTOCOL,
                client: "rovik-ide 0.1.0".into(),
                device: "laptop".into(),
                token: Secret("tok".into()),
            },
            Frame::Welcome {
                protocol: PROTOCOL,
                min_protocol: 1,
                daemon: "rovik-agentd 0.1.0".into(),
                boot: "b1".into(),
                features: vec!["blobs".into()],
            },
            Frame::Call {
                id: 1,
                req: Request::AgentStatus,
            },
            Frame::Reply {
                id: 1,
                res: Ok(Response::Bool(true)),
            },
            Frame::Reply {
                id: 2,
                res: Err(Fail::new(FailCode::TooLarge, "Over 1 MB")),
            },
            Frame::Event {
                seq: 9,
                ev: Event::Resync {
                    dialog_id: "d".into(),
                },
            },
            Frame::Subscribe { since: None },
            Frame::Subscribe {
                since: Some(("b1".into(), 9)),
            },
            Frame::Subscribed {
                replayed: 3,
                complete: false,
            },
            Frame::Ping,
            Frame::Pong,
        ],
    );
}

#[test]
fn every_request_round_trips() {
    let hunk = Hunk {
        orig_start: 1,
        deleted: vec!["a".into()],
        added: vec!["b".into()],
    };
    covers(
        "t",
        &[
            Request::AgentStart(start()),
            Request::AgentStart(StartRequest {
                start: StartKind::RunChecks {
                    record_id: "r".into(),
                },
                options: None,
                config: None,
                ..start()
            }),
            Request::AgentStart(StartRequest {
                start: StartKind::CheckMerged {
                    files: vec!["src/main.rs".into()],
                    diffs: vec!["@@ -1 +1 @@\n-fn main() {}\n+fn main() { run() }".into()],
                },
                ..start()
            }),
            Request::AgentStop {
                dialog_id: "d".into(),
            },
            Request::AgentPause {
                dialog_id: "d".into(),
                host: "server".into(),
            },
            Request::AgentResolveApproval {
                dialog_id: "d".into(),
                tool_call_id: "c".into(),
                decision: ApprovalDecision::Partial {
                    content: "b\n".into(),
                    total: 2,
                    rejected: vec![hunk],
                },
            },
            Request::AgentAnswerQuestions {
                dialog_id: "d".into(),
                tool_call_id: "q".into(),
                outcome: QuestionsOutcome::Answers {
                    answers: vec![
                        QuestionAnswer::Option { label: "A".into() },
                        QuestionAnswer::Custom { text: "C".into() },
                        QuestionAnswer::Agent,
                    ],
                },
            },
            Request::AgentStatus,
            Request::AgentConfigure(config()),
            Request::AgentSetEffort {
                profile_id: "p::m".into(),
                options: Some(options()),
                dialog_id: None,
            },
            Request::ChatChecklist {
                dialog_id: "d".into(),
            },
            Request::ChatDismissChecklist {
                dialog_id: "d".into(),
                message_id: "m".into(),
            },
            Request::WorkloadFocus {
                dialog_id: Some("d".into()),
            },
            Request::ChatRename {
                dialog_id: "d".into(),
                title: "New title".into(),
            },
            Request::ChatUpdateMessage {
                message_id: "m".into(),
                patch: MessagePatch {
                    content: Some("x".into()),
                    tool_calls: Some(vec![ToolCall {
                        id: "c".into(),
                        name: "read".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    }]),
                    failed: Some(true),
                    finalize: true,
                    ..Default::default()
                },
            },
            Request::ChatSetModel {
                dialog_id: "d".into(),
                model: ChatModel {
                    provider_id: "p".into(),
                    model_id: "m".into(),
                    effort: Some("high".into()),
                },
            },
            Request::ChatDelete {
                dialog_id: "d".into(),
            },
            Request::ChatChangesSince {
                dialog_id: "d".into(),
                rev: 0,
                cursor: None,
            },
            Request::ChatChangesSince {
                dialog_id: "d".into(),
                rev: 41,
                cursor: changes_page().next,
            },
            Request::ChatOwner {
                dialog_id: "d".into(),
            },
            Request::ChatRelease {
                dialog_id: "d".into(),
                move_id: None,
            },
            Request::ChatRelease {
                dialog_id: "d".into(),
                move_id: Some("m".into()),
            },
            Request::ChatForget {
                dialog_id: "d".into(),
            },
            Request::BlobsMissing {
                ids: vec!["a".repeat(64), "b".repeat(64)],
            },
            Request::ClmStatus {
                dialog_id: "d".into(),
            },
            Request::ClmSetMode {
                dialog_id: "d".into(),
                mode: "clm".into(),
            },
            Request::ClmReset {
                dialog_id: "d".into(),
            },
            Request::ClmListRevisions {
                dialog_id: "d".into(),
            },
            Request::ClmReadRevision {
                dialog_id: "d".into(),
                number: 2,
            },
            Request::ClmRestoreRevision {
                dialog_id: "d".into(),
                number: 2,
            },
            Request::ProjectEnsure {
                project: Project {
                    id: "p".into(),
                    name: "app".into(),
                    subpath: "web".into(),
                },
                settings: Some(json!({ "checks": ["npm test"] })),
                excludes: vec!["*.local".into()],
            },
            Request::ProjectEnsure {
                project: Project {
                    id: "p".into(),
                    name: "app".into(),
                    subpath: String::new(),
                },
                settings: None,
                excludes: Vec::new(),
            },
            Request::WorkSync {
                dialog_id: "d".into(),
                project: "p".into(),
                snapshot: "0123456789abcdef0123456789abcdef01234567".into(),
                excludes: vec!["*.local".into()],
                changes: None,
            },
            Request::WorkSync {
                dialog_id: "d".into(),
                project: "p".into(),
                snapshot: "0123456789abcdef0123456789abcdef01234567".into(),
                excludes: Vec::new(),
                changes: Some("89abcdef0123456789abcdef0123456789abcdef".into()),
            },
            Request::WorkResult {
                dialog_id: "d".into(),
            },
            Request::WorkResults,
            Request::WorkStatus {
                dialog_id: "d".into(),
            },
            Request::WorkFile {
                dialog_id: "d".into(),
                path: ".rovik/plans/plan.md".into(),
            },
            Request::WorkDiscard {
                dialog_id: "d".into(),
            },
            Request::WorkConflictsToChat {
                dialog_id: "d".into(),
                snapshot: "0123456789abcdef0123456789abcdef01234567".into(),
                files: vec!["src/main.rs".into()],
            },
            Request::WorkUndoMerge {
                dialog_id: "d".into(),
                files: None,
            },
            Request::WorkUndoMerge {
                dialog_id: "d".into(),
                files: Some(vec!["src/main.rs".into()]),
            },
            Request::WorkRecheck {
                dialog_id: "d".into(),
                project: "p".into(),
                snapshot: "0123456789abcdef0123456789abcdef01234567".into(),
                excludes: Vec::new(),
            },
            Request::WorkLanded {
                dialog_id: "d".into(),
                commit: "0123456789abcdef0123456789abcdef01234567".into(),
                paths: vec!["src/main.rs".into()],
            },
            Request::HostInfo,
            Request::KeysList,
            Request::KeysRemove { provider: None },
            Request::KeysRemove {
                provider: Some("openai".into()),
            },
            Request::Storage,
        ],
    );
}

#[test]
fn every_response_round_trips() {
    let status = AgentStatus {
        runs: vec![RunningRun {
            dialog_id: "d".into(),
            run_id: "r".into(),
        }],
        subagents: vec![
            RunningSubagent {
                dialog_id: "kid".into(),
                parent_dialog_id: "d".into(),
                parent_call_id: "t1".into(),
                info: Some(ChildInfo {
                    kind: "scout".into(),
                    goal: "Look".into(),
                    model: "Test/m".into(),
                    started_at: 1,
                    tool_calls: 2,
                }),
            },
            RunningSubagent {
                dialog_id: "kid2".into(),
                parent_dialog_id: "d".into(),
                parent_call_id: "t2".into(),
                info: None,
            },
        ],
        approvals: vec![approval()],
        questions: vec![questions()],
    };
    covers(
        "t",
        &[
            Response::Done,
            Response::Bool(false),
            Response::RunId(Some("r".into())),
            Response::RunId(None),
            Response::Count(2),
            Response::AgentStatus(status),
            Response::Checklist(Some(checklist())),
            Response::Checklist(None),
            Response::ClmStatus(clm_status()),
            Response::Revisions(vec![RevisionRow {
                number: 1,
                kind: "context".into(),
                created_at: 5,
                tokens: 10,
            }]),
            Response::Text("revision text".into()),
            Response::HostInfo(HostInfo {
                version: "0.1.0".into(),
                protocol: PROTOCOL,
                min_protocol: 1,
                os: "linux".into(),
                arch: "x86_64".into(),
                user: "me".into(),
                virtual_files: false,
                virtual_files_problem: Some("/dev/fuse is missing".into()),
                chrome: Some("/usr/bin/chromium".into()),
                user_namespaces: true,
                install: Some(Install {
                    admins: vec!["me".into()],
                }),
                other_daemons: 2,
            }),
            Response::HostInfo(HostInfo::default()),
            Response::Ids(vec!["a".repeat(64)]),
            Response::Ids(Vec::new()),
            Response::Synced(Synced {
                root: "/home/me/.local/share/rovik/work/d".into(),
                state: SyncState::Synced,
                files: Vec::new(),
            }),
            Response::Synced(Synced {
                root: "/home/me/.local/share/rovik/work/d".into(),
                state: SyncState::Marked,
                files: vec!["src/main.rs".into()],
            }),
            Response::ChatResult(result(Some("Not landed: the run was stopped."))),
            Response::ChatResult(ChatResult {
                marked: vec!["src/main.rs".into()],
                ..result(None)
            }),
            Response::ChatResult(ChatResult {
                unchecked: true,
                ..result(None)
            }),
            Response::ChatResults(vec![result(None)]),
            Response::ChatResults(Vec::new()),
            Response::Changes(vec![
                WorkChange {
                    path: "src/new.rs".into(),
                    change: Change::Added,
                    from: None,
                    merged: false,
                },
                WorkChange {
                    path: "src/main.rs".into(),
                    change: Change::Changed,
                    from: None,
                    merged: true,
                },
                WorkChange {
                    path: "old.rs".into(),
                    change: Change::Deleted,
                    from: None,
                    merged: false,
                },
                WorkChange {
                    path: "src/moved.rs".into(),
                    change: Change::Moved,
                    from: Some("moved.rs".into()),
                    merged: false,
                },
            ]),
            Response::WorkFile(WorkFile {
                base: Some("fn main() {}\n".into()),
                chat: None,
            }),
            Response::Handed(Handed {
                files: vec!["src/main.rs".into()],
            }),
            Response::UndoneMerge(UndoneMerge {
                restored: vec!["src/main.rs".into()],
                changed_since: vec!["src/lib.rs".into()],
            }),
            Response::ChatChanges(changes_page()),
            Response::ChatChanges(ChangesPage::default()),
            Response::Ownership(None),
            Response::Ownership(Some(Ownership {
                move_id: Some("m".into()),
                released: true,
                rev: 41,
            })),
            Response::Ownership(Some(Ownership::default())),
            Response::Keys(vec![KeyInfo {
                provider: "openai".into(),
                base_url: "https://api.openai.com/v1".into(),
            }]),
            Response::Keys(Vec::new()),
            Response::Storage(Storage {
                projects: 1 << 30,
                work: 2,
                data: 3,
                tmp: 0,
            }),
        ],
    );
}

/// A key's provider and base URL go to the hosts page; the key never does.
#[test]
fn the_key_list_carries_no_key() {
    let json = round_trip(&Response::Keys(vec![KeyInfo {
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
    }]));
    assert_eq!(
        json,
        json!({ "t": "keys", "v": [{ "provider": "openai", "baseUrl": "https://api.openai.com/v1" }] })
    );
}

/// The bundle that moves a chat, and what its import answers (6.8).
#[test]
fn a_chats_bundle_and_its_import_round_trip() {
    let bundle = ChatBundle {
        dialog_id: "d".into(),
        rows: changes_page().changes,
        project: Project {
            id: "p".into(),
            name: "app".into(),
            subpath: "web".into(),
        },
        settings: Some(json!({ "checks": ["npm test"] })),
        excludes: vec!["*.log".into()],
        snapshot: "a".repeat(40),
        changes: Some("b".repeat(40)),
        pending: Some("Not landed: the run was stopped.".into()),
    };
    let json = round_trip(&bundle);
    assert_eq!(json["rows"]["dialogs"][0]["id"], "d");
    let bare = ChatBundle {
        settings: None,
        excludes: Vec::new(),
        changes: None,
        pending: None,
        ..bundle
    };
    let json = round_trip(&bare);
    for absent in ["settings", "excludes", "changes", "pending"] {
        assert!(json.get(absent).is_none(), "{absent} in {json}");
    }
    let json = round_trip(&Imported {
        rev: 7,
        already: false,
    });
    assert_eq!(json, json!({ "rev": 7 }));
    round_trip(&Imported {
        rev: 7,
        already: true,
    });
}

#[test]
fn every_changed_table_round_trips_in_the_order_a_pull_reads_them() {
    let tables = [
        ChangedTable::Dialogs,
        ChangedTable::ClmState,
        ChangedTable::ClmRevisions,
        ChangedTable::Messages,
    ];
    for table in tables {
        round_trip(&table);
    }
    assert!(tables.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn every_event_round_trips() {
    covers(
        "t",
        &[
            Event::Agent(AgentEvent::RunStarted {
                dialog_id: "d".into(),
                run_id: "r".into(),
            }),
            Event::ResultReady(result(None)),
            Event::ChatChanged {
                dialog_id: "d".into(),
                rev: 41,
            },
            Event::SyncState(ChatSync {
                dialog_id: "d".into(),
                state: SyncState::Conflicts,
                files: vec!["src/main.rs".into()],
            }),
            Event::HostNotice {
                kind: "drain".into(),
                message: "Rovik is being upgraded.".into(),
            },
            Event::Resync {
                dialog_id: "d".into(),
            },
        ],
    );
}

#[test]
fn every_sync_state_round_trips() {
    for state in [
        SyncState::Synced,
        SyncState::Marked,
        SyncState::Conflicts,
        SyncState::Unmergeable,
    ] {
        round_trip(&state);
    }
    let older: Synced = serde_json::from_value(json!({ "root": "/w" })).unwrap();
    assert_eq!(older.state, SyncState::Synced, "a daemon from T03");
}

#[test]
fn every_fail_code_round_trips() {
    for code in [
        FailCode::Unreachable,
        FailCode::NotOwner,
        FailCode::Busy,
        FailCode::Version,
        FailCode::BadRequest,
        FailCode::TooLarge,
        FailCode::Internal,
    ] {
        round_trip(&Fail::new(code, "why"));
    }
    assert_eq!(
        serde_json::to_value(Fail::new(FailCode::NotOwner, "Moved")).unwrap(),
        json!({ "code": "notOwner", "message": "Moved" })
    );
}

/// The shapes on the wire, as section 4.1 describes them.
#[test]
fn frames_have_the_shapes_of_4_1() {
    assert_eq!(
        round_trip(&Frame::Call {
            id: 7,
            req: Request::AgentStop {
                dialog_id: "d".into()
            },
        }),
        json!({ "t": "call", "id": 7, "req": { "t": "agentStop", "dialogId": "d" } })
    );
    assert_eq!(
        round_trip(&Frame::Reply {
            id: 7,
            res: Ok(Response::Bool(true)),
        }),
        json!({ "t": "reply", "id": 7, "res": { "Ok": { "t": "bool", "v": true } } })
    );
    assert_eq!(
        round_trip(&Frame::Event {
            seq: 3,
            ev: Event::Agent(AgentEvent::ApprovalSettled {
                dialog_id: "d".into(),
                tool_call_id: "c".into(),
            }),
        }),
        json!({
            "t": "event",
            "seq": 3,
            "ev": { "t": "agent", "type": "approvalSettled", "dialogId": "d", "toolCallId": "c" }
        })
    );
    assert_eq!(
        round_trip(&Frame::Subscribe {
            since: Some(("b".into(), 4))
        }),
        json!({ "t": "subscribe", "since": ["b", 4] })
    );
}

/// A request's fields are its Tauri command's arguments (D3), so the app
/// sends a remote chat's call as the frontend made it.
#[test]
fn a_requests_fields_are_its_commands_arguments() {
    let mut json = round_trip(&Request::AgentResolveApproval {
        dialog_id: "d".into(),
        tool_call_id: "c".into(),
        decision: ApprovalDecision::Allow,
    });
    json.as_object_mut().unwrap().remove("t");
    assert_eq!(
        json,
        json!({ "dialogId": "d", "toolCallId": "c", "decision": { "kind": "allow" } })
    );
    let start: Value = serde_json::to_value(Request::AgentStart(start())).unwrap();
    assert_eq!(start["t"], "agentStart");
    assert_eq!(
        start["dialogId"], "d",
        "the start's own fields, next to the tag"
    );
    assert_eq!(start["start"]["kind"], "send");
}

/// `status: null` clears a message's status, so it must stay `null` after
/// a trip through the daemon, apart from an absent status (K39).
#[test]
fn a_patchs_null_status_survives_a_round_trip() {
    let read = |json: Value| serde_json::from_value::<Patch>(json).unwrap();
    assert_eq!(read(json!({ "content": "x" })).status, None);
    assert_eq!(read(json!({ "status": null })).status, Some(None));
    assert_eq!(
        read(json!({ "status": { "text": "Waiting", "since": 5 } })).status,
        Some(Some(Status {
            text: "Waiting".into(),
            until: None,
            since: Some(5),
        }))
    );
    for patch in [
        Patch::default(),
        Patch {
            status: Some(None),
            ..Default::default()
        },
        Patch {
            content: Some("x".into()),
            status: Some(Some(Status::text("Waiting"))),
            ..Default::default()
        },
    ] {
        round_trip(&patch);
    }
    assert_eq!(
        round_trip(&Patch {
            status: Some(None),
            ..Default::default()
        }),
        json!({ "status": null })
    );
    assert_eq!(round_trip(&Patch::default()), json!({}));
    // Inside an event too, which serde reads through a buffer.
    let event = round_trip(&AgentEvent::MessagePatched {
        dialog_id: "d".into(),
        message_id: "m".into(),
        patch: Patch {
            status: Some(None),
            ..Default::default()
        },
    });
    assert_eq!(event["patch"], json!({ "status": null }));
}

/// The events as test-fixtures/agent-events.json has them, which
/// src/lib/agentRuns.test.ts feeds to the frontend: each one reads into
/// `AgentEvent` and writes back the same, and there's one of each.
#[test]
fn the_shared_event_fixtures_round_trip_unchanged() {
    let fixtures: Value =
        serde_json::from_str(include_str!("../../../../test-fixtures/agent-events.json")).unwrap();
    let events = fixtures["events"].as_array().unwrap();
    let mut seen = BTreeSet::new();
    for json in events {
        let event: AgentEvent = serde_json::from_value(json.clone())
            .unwrap_or_else(|e| panic!("{json} doesn't read: {e}"));
        assert_eq!(&serde_json::to_value(&event).unwrap(), json);
        round_trip(&event);
        seen.insert(json["type"].as_str().unwrap().to_string());
    }
    assert_eq!(
        seen,
        tags::<AgentEvent>("type"),
        "event types without a fixture"
    );
}

/// K02: a run's file writes and notices say which chat they're from.
#[test]
fn file_writes_and_notices_name_their_chat() {
    let notice = AgentEvent::Notice {
        dialog_id: Some("d".into()),
        kind: "info".into(),
        title: "T".into(),
        message: "M".into(),
    };
    assert_eq!(round_trip(&notice)["dialogId"], "d");
    // None of the app's notices about no chat changes shape.
    let general = round_trip(&AgentEvent::Notice {
        dialog_id: None,
        kind: "info".into(),
        title: "T".into(),
        message: "M".into(),
    });
    assert!(general.get("dialogId").is_none(), "{general}");
    let written = round_trip(&AgentEvent::FileWritten {
        dialog_id: Some("d".into()),
        abs_path: "/p/a".into(),
        before: None,
        content: "x".into(),
        reviewed: false,
        deleted: true,
    });
    assert_eq!(written["dialogId"], "d");
    assert_eq!(written["deleted"], true);
}

#[test]
fn a_tokens_debug_doesnt_show_it() {
    let hello = Frame::Hello {
        protocol: PROTOCOL,
        client: "c".into(),
        device: "d".into(),
        token: Secret("tok-123".into()),
    };
    assert!(!format!("{hello:?}").contains("tok-123"));
    assert_eq!(serde_json::to_value(&hello).unwrap()["token"], "tok-123");
}

#[test]
fn a_reply_reads_as_its_requests_value() {
    assert_eq!(bool::try_from(Response::Bool(true)), Ok(true));
    assert_eq!(<()>::try_from(Response::Done), Ok(()));
    assert_eq!(
        Option::<String>::try_from(Response::RunId(Some("r".into()))),
        Ok(Some("r".into()))
    );
    assert_eq!(String::try_from(Response::Text("t".into())), Ok("t".into()));
    assert_eq!(
        Vec::<String>::try_from(Response::Ids(vec!["a".into()])),
        Ok(vec!["a".into()])
    );
    let wrong = bool::try_from(Response::Done).unwrap_err();
    assert_eq!(wrong.code, FailCode::Internal);
    assert!(<()>::try_from(Response::Bool(true)).is_err());
}

#[test]
fn a_patch_then_a_newer_one_keeps_what_the_newer_leaves_alone() {
    let first = Patch {
        content: Some("Hel".into()),
        status: Some(Some(Status::text("Writing"))),
        meta: Some(json!({ "a": 1 })),
        ..Default::default()
    };
    let second = Patch {
        content: Some("Hello".into()),
        finalized: Some(true),
        ..Default::default()
    };
    let both = first.clone().then(second);
    assert_eq!(both.content.as_deref(), Some("Hello"));
    assert_eq!(both.finalized, Some(true));
    assert_eq!(both.status, Some(Some(Status::text("Writing"))));
    assert_eq!(both.meta, Some(json!({ "a": 1 })));
    // A cleared status clears it.
    let cleared = first.then(Patch {
        status: Some(None),
        ..Default::default()
    });
    assert_eq!(cleared.status, Some(None));
}

/// Every event names its chat, but what applies to the settings, and a
/// notice about no chat: those go to every computer of a daemon.
#[test]
fn an_event_names_its_chat() {
    let fixtures: Value =
        serde_json::from_str(include_str!("../../../../test-fixtures/agent-events.json")).unwrap();
    for json in fixtures["events"].as_array().unwrap() {
        let event: AgentEvent = serde_json::from_value(json.clone()).unwrap();
        let expected = match json["type"].as_str().unwrap() {
            "reasoningLearned" | "apiRouteLearned" => None,
            "approvalRequested" | "questionsAsked" => json["request"]["dialogId"].as_str(),
            _ => json["dialogId"].as_str(),
        };
        assert_eq!(event.dialog_id(), expected, "{json}");
    }
}

/// Fields added after a type first shipped are optional: a peer that
/// doesn't send them is read as before, and they're left out when unset.
#[test]
fn later_fields_are_optional() {
    let mut start = serde_json::to_value(start()).unwrap();
    start.as_object_mut().unwrap().remove("projectSettings");
    let start: StartRequest = serde_json::from_value(start).unwrap();
    assert_eq!(start.project_settings, None);
    assert!(serde_json::to_value(&start)
        .unwrap()
        .get("projectSettings")
        .is_none());

    let mut approval = serde_json::to_value(approval()).unwrap();
    approval.as_object_mut().unwrap().remove("remote");
    let approval: ApprovalRequest = serde_json::from_value(approval).unwrap();
    assert!(!approval.remote);
    assert!(serde_json::to_value(&approval)
        .unwrap()
        .get("remote")
        .is_none());
}

/// D11: the models a configuration names, each subagent kind's included,
/// and the same configuration without their keys.
#[test]
fn a_configuration_without_its_keys() {
    let mut config = config();
    for model in config.models_mut() {
        model.api_key = format!("sk-{}", model.id);
    }
    let named: Vec<&str> = config.models().map(|m| m.id.as_str()).collect();
    assert!(named.len() >= 2, "{named:?}");
    let keyless = config.without_keys();
    assert!(keyless.models().all(|m| m.api_key.is_empty()));
    assert_eq!(keyless.models().count(), named.len());
    assert!(config.models().all(|m| !m.api_key.is_empty()), "untouched");
}

/// The bodies of `/packs` (4.4), and the query of a push.
#[test]
fn pack_bodies_round_trip() {
    use crate::packs::{pushable, Fetch, Pushed, Refs, Update};
    let id = "0123456789abcdef0123456789abcdef01234567";
    let refs = Refs {
        refs: [("refs/rovik/chats/c/snapshot".to_string(), id.to_string())].into(),
    };
    assert_eq!(
        round_trip(&refs),
        json!({ "refs": { "refs/rovik/chats/c/snapshot": id } })
    );
    let fetch = Fetch {
        wants: vec![id.into()],
        haves: vec![],
    };
    assert_eq!(round_trip(&fetch), json!({ "wants": [id], "haves": [] }));
    assert_eq!(round_trip(&Pushed { objects: 3 }), json!({ "objects": 3 }));

    for old in [None, Some(id.to_string())] {
        let update = Update {
            name: "refs/rovik/chats/c/snapshot".into(),
            old,
            new: id.into(),
        };
        assert_eq!(Update::parse(&update.query()), Some(update));
    }
    for name in [
        "refs/heads/main",
        "refs/rovik/../x",
        "refs/rovik/a b",
        "refs/rovik/x.lock",
    ] {
        assert!(!pushable(name), "{name}");
    }
    let unsafe_query = format!("ref=refs/rovik/a&b=c&new={id}&ref=refs/heads/main");
    assert_eq!(Update::parse(&unsafe_query), None);
    assert_eq!(Update::parse("ref=refs/rovik/a&new=HEAD"), None);
}

#[test]
fn a_result_from_a_daemon_that_numbers_none_reads_as_zero() {
    let older: ChatResult = serde_json::from_value(json!({
        "dialogId": "d",
        "commit": "0123456789abcdef0123456789abcdef01234567",
        "outcome": "done",
    }))
    .unwrap();
    assert_eq!(older.seq, 0, "a daemon from T05");
}

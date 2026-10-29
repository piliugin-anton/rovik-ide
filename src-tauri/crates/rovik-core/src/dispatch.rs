//! What can be done to a chat, in one place (D3): the app's commands call
//! `dispatch` for a chat on this computer, and the daemon for every call it
//! gets, so the two serve the same operations the same way. The daemon adds
//! the Work and Host groups of section 4.2.
//!
//! A chat owned elsewhere (its `location`, chat_db/location.rs) is refused
//! with `NotOwner` (4.2, K21): one on a host, whose rows here only its
//! mirror writes; one moving there, which nothing may change until the move
//! is over, though the run that pauses for it can still be stopped or
//! answered; and on a daemon, one its computer took back. Their rows can
//! still be read.

use crate::agent_run;
use crate::chat_db::location::{self, Location};
use crate::chat_db::{api, attachments, sync};
use crate::clm;
use crate::command_tool::background;
use crate::core::Core;
use crate::workload;
use rovik_protocol::{Access, Fail, FailCode, Request, Response, Target};

/// Serves a request of the Agent, Chat or CLM group. A failure carries the
/// error text the operation gives, which is what the app's command returns.
pub async fn dispatch(core: &Core, request: Request) -> Result<Response, Fail> {
    if let Some((target, access)) = request.target() {
        check_owner(core, target, access).await?;
    }
    let (db, vfs) = (&core.db, &core.vfs);
    let done = |()| Response::Done;
    match request {
        Request::AgentStart(start) => agent_run::agent_start(core, *start)
            .await
            .map(Response::RunId),
        Request::AgentStop { dialog_id } => {
            Ok(Response::Bool(agent_run::agent_stop(core, dialog_id).await))
        }
        Request::AgentPause { dialog_id, host } => agent_run::agent_pause(core, dialog_id, host)
            .await
            .map(Response::Bool),
        Request::AgentResolveApproval {
            dialog_id,
            tool_call_id,
            decision,
        } => Ok(Response::Bool(agent_run::agent_resolve_approval(
            core,
            dialog_id,
            tool_call_id,
            decision,
        ))),
        Request::AgentAnswerQuestions {
            dialog_id,
            tool_call_id,
            outcome,
        } => Ok(Response::Bool(agent_run::agent_answer_questions(
            core,
            dialog_id,
            tool_call_id,
            outcome,
        ))),
        Request::AgentSteer { dialog_id, message } => {
            agent_run::agent_steer(core, dialog_id, message)
                .await
                .map(Response::Steer)
        }
        Request::AgentUnsteer {
            dialog_id,
            message_id,
        } => Ok(Response::Bool(agent_run::agent_unsteer(
            core, dialog_id, message_id,
        ))),
        Request::AgentStatus => Ok(Response::AgentStatus(agent_run::agent_status(core))),
        Request::AgentConfigure(config) => {
            agent_run::agent_configure(core, *config);
            Ok(Response::Done)
        }
        Request::AgentSetEffort {
            profile_id,
            options,
            dialog_id,
        } => Ok(Response::Count(agent_run::agent_set_effort(
            core, profile_id, options, dialog_id,
        ))),
        Request::ChatChecklist { dialog_id } => agent_run::chat_checklist(core, dialog_id)
            .await
            .map(Response::Checklist),
        Request::ChatDismissChecklist {
            dialog_id,
            message_id,
        } => agent_run::chat_dismiss_checklist(core, dialog_id, message_id)
            .await
            .map(Response::Checklist),
        Request::BackgroundTasks { dialog_id } => {
            Ok(Response::BackgroundTasks(background::tasks(&dialog_id)))
        }
        // It waits until what it killed is gone.
        Request::BackgroundStop { dialog_id, id } => {
            crate::rt::spawn_blocking(move || background::stop_for_user(&dialog_id, &id))
                .await
                .map(Response::Bool)
                .map_err(|e| e.to_string())
        }
        Request::ConnectorProbe(args) => crate::connectors::probe(args)
            .await
            .map(Response::ConnectorProbe),
        Request::ConnectorAuthStatus(args) => crate::connectors::auth_status(args)
            .await
            .map(Response::ConnectorAuth),
        Request::ConnectorSetup(op) => crate::connectors::setup(op)
            .await
            .map(Response::ConnectorSetup),
        Request::ConnectorCommands { args, cwd } => crate::connectors::commands(args, cwd)
            .await
            .map(Response::ConnectorCommands),
        Request::WorkloadFocus { dialog_id } => {
            workload::workload_focus(dialog_id);
            Ok(Response::Done)
        }

        Request::ChatRename { dialog_id, title } => api::chat_rename_dialog(db, dialog_id, title)
            .await
            .map(done),
        Request::ChatUpdateMessage { message_id, patch } => {
            api::chat_update_message(db, message_id, patch)
                .await
                .map(done)
        }
        Request::ChatSetModel { dialog_id, model } => {
            api::chat_set_model(db, dialog_id, model).await.map(done)
        }
        Request::ChatDelete { dialog_id } => {
            api::chat_delete_dialog(core, dialog_id).await.map(done)
        }
        Request::BlobsMissing { ids } => {
            attachments::blobs_missing(db, ids).await.map(Response::Ids)
        }
        Request::ChatChangesSince {
            dialog_id,
            rev,
            cursor,
        } => sync::pull(db, dialog_id, rev, cursor)
            .await
            .map(Response::ChatChanges),

        Request::ClmStatus { dialog_id } => clm::status(db, vfs, dialog_id)
            .await
            .map(Response::ClmStatus),
        Request::ClmSetMode { dialog_id, mode } => {
            clm::set_mode(db, dialog_id, mode).await.map(done)
        }
        Request::ClmReset { dialog_id } => clm::reset(db, vfs, &dialog_id).await.map(done),
        Request::ClmListRevisions { dialog_id } => clm::list_revisions(db, dialog_id)
            .await
            .map(Response::Revisions),
        Request::ClmReadRevision { dialog_id, number } => {
            clm::revision_text(db, vfs, &dialog_id, number)
                .await
                .map(Response::Text)
        }
        Request::ClmRestoreRevision { dialog_id, number } => {
            clm::restore_revision(db, vfs, &dialog_id, number)
                .await
                .map(|_| Response::Done)
        }

        // The daemon's own groups: it answers these itself.
        Request::ChatOwner { .. }
        | Request::ChatRelease { .. }
        | Request::ChatForget { .. }
        | Request::ProjectEnsure { .. }
        | Request::WorkSync { .. }
        | Request::WorkResult { .. }
        | Request::WorkResults
        | Request::WorkRecheck { .. }
        | Request::WorkStatus { .. }
        | Request::WorkFile { .. }
        | Request::WorkDiscard { .. }
        | Request::WorkConflictsToChat { .. }
        | Request::WorkUndoMerge { .. }
        | Request::WorkLanded { .. }
        | Request::HostInfo
        | Request::KeysList
        | Request::KeysRemove { .. }
        | Request::McpSecretsKept
        | Request::McpTest { .. }
        | Request::Storage
        | Request::CleanUp
        | Request::PageOpened { .. }
        | Request::AutonomyConfigure(_)
        | Request::DeliveryTest { .. }
        | Request::TaskStatus => {
            return Err(Fail::new(
                FailCode::BadRequest,
                "Only a daemon answers this request.",
            ))
        }
    }
    .map_err(|message| Fail::new(FailCode::Internal, message))
}

/// Refuses what `access` would do to a chat owned elsewhere.
async fn check_owner(core: &Core, target: Target<'_>, access: Access) -> Result<(), Fail> {
    if access == Access::Read {
        return Ok(());
    }
    let located = match target {
        Target::Chat(id) => {
            let id = id.to_string();
            core.db.read(move |c| location::of_chat(c, &id)).await
        }
        Target::Message(id) => {
            let id = id.to_string();
            core.db.read(move |c| location::of_message(c, &id)).await
        }
    };
    let located = located.map_err(|message| Fail::new(FailCode::Internal, message))?;
    match refusal(&located, access) {
        Some(why) => Err(Fail::new(FailCode::NotOwner, why)),
        None => Ok(()),
    }
}

/// Why `access` isn't allowed to a chat at `location`, if it isn't.
pub fn refusal(location: &Location, access: Access) -> Option<String> {
    match (location, access) {
        (Location::Here, _) | (_, Access::Read) | (Location::Moving(_), Access::Run) => None,
        (Location::Moving(host), _) => Some(format!(
            "This chat is moving to {host}: it can't change until the move is over."
        )),
        (Location::Host(host), _) => Some(format!(
            "This chat is on {host}: it changes there, and this copy follows it."
        )),
        (Location::Released, _) => Some("This chat went back to its computer.".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_run::{
        AgentConfig, ApprovalDecision, ModelProfile, StartKind, StartRequest, UserMessage,
    };
    use crate::chat_db::store::{self, ChatModel, MessagePatch, NewDialog, NewMessage, ToolCall};
    use crate::testing::{model, Reply, TestCore};
    use rovik_protocol::plan::QuestionsOutcome;
    use serde_json::json;
    use std::time::Duration;

    /// A chat with these messages, in a fresh core.
    async fn chat(messages: Vec<NewMessage>) -> (TestCore, String) {
        let test = TestCore::new();
        let id = uuid::Uuid::new_v4().to_string();
        let dialog = NewDialog {
            id: id.clone(),
            title: "Chat".into(),
            created_at: 0,
            updated_at: 0,
            project_id: None,
        };
        let d = id.clone();
        test.core
            .db
            .write(move |c| {
                store::create_dialog(c, &dialog)?;
                store::append_messages(c, &d, &messages).map(|_| ())
            })
            .await
            .unwrap();
        (test, id)
    }

    fn message(id: &str, role: &str, content: &str) -> NewMessage {
        NewMessage {
            id: id.into(),
            role: role.into(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            meta: None,
            error: None,
            failed: false,
            attachments: Vec::new(),
            created_at: 1,
            finalized: true,
        }
    }

    fn start(dialog_id: &str, url: &str, start: StartKind) -> StartRequest {
        StartRequest {
            dialog_id: dialog_id.into(),
            start,
            profile: ModelProfile {
                id: "p::m".into(),
                label: "Test/m".into(),
                model: "m".into(),
                base_url: url.into(),
                api_key: String::new(),
                stream: true,
                vision: false,
                pdf: false,
                context_tokens: Some(100_000),
                no_tools: false,
                first_byte_timeout_ms: None,
                api_route: None,
                connector: None,
            },
            options: None,
            mode: Some("agent".into()),
            context_mode: Some("trimming".into()),
            root: None,
            current_user_content: None,
            local_date: None,
            config: None,
            isolate: Some(false),
            project_settings: None,
            mcp_secrets: Default::default(),
        }
    }

    async fn call(core: &Core, request: Request) -> Response {
        dispatch(core, request).await.unwrap()
    }

    async fn row(core: &Core, id: &str) -> store::MessageRow {
        let id = id.to_string();
        core.db
            .read(move |c| {
                let page = store::get_messages_around(c, &id, 0, 0)?;
                Ok(page.messages.into_iter().find(|m| m.id == id))
            })
            .await
            .unwrap()
            .expect("the message")
    }

    #[tokio::test]
    async fn agent_start_runs_and_names_its_run() {
        let (test, id) = chat(Vec::new()).await;
        let (url, _) = model(vec![Reply::Text("Hi")]).await;
        let send = StartKind::Send {
            message: UserMessage {
                id: "u".into(),
                content: "Hello".into(),
                meta: None,
                attachments: Vec::new(),
            },
        };
        let Response::RunId(Some(run_id)) = call(
            &test.core,
            Request::AgentStart(Box::new(start(&id, &url, send))),
        )
        .await
        else {
            panic!("no run id");
        };
        for _ in 0..400 {
            let ended = test.events.lock().unwrap().iter().any(|e| {
                e["type"] == "runEnded" && e["runId"] == run_id.as_str() && e["outcome"] == "done"
            });
            if ended {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("the run didn't end: {:#?}", test.events.lock().unwrap());
    }

    #[tokio::test]
    async fn agent_start_with_nothing_to_do_names_no_run() {
        let (test, id) = chat(vec![message("u", "user", "Hi")]).await;
        let resume = start(&id, "http://127.0.0.1:9", StartKind::Resume);
        assert_eq!(
            call(&test.core, Request::AgentStart(Box::new(resume))).await,
            Response::RunId(None)
        );
    }

    #[tokio::test]
    async fn agent_start_fails_with_the_operations_reason() {
        let (test, id) = chat(Vec::new()).await;
        let mut send = start(
            &id,
            "http://127.0.0.1:9",
            StartKind::Send {
                message: UserMessage {
                    id: "u".into(),
                    content: "Hello".into(),
                    meta: None,
                    attachments: vec!["no-such-file".into()],
                },
            },
        );
        send.config = Some(AgentConfig::default());
        let fail = dispatch(&test.core, Request::AgentStart(Box::new(send)))
            .await
            .unwrap_err();
        assert_eq!(fail.code, FailCode::Internal);
        assert!(fail.message.contains("no longer available"), "{fail}");
    }

    #[tokio::test]
    async fn agent_stop_says_whether_a_run_was_there() {
        let (test, id) = chat(Vec::new()).await;
        assert_eq!(
            call(&test.core, Request::AgentStop { dialog_id: id }).await,
            Response::Bool(false)
        );
    }

    #[tokio::test]
    async fn agent_pause_says_whether_the_run_paused_for_the_move() {
        let (test, id) = chat(Vec::new()).await;
        let request = Request::AgentPause {
            dialog_id: id,
            host: "server".into(),
        };
        assert_eq!(call(&test.core, request).await, Response::Bool(false));
    }

    #[tokio::test]
    async fn agent_resolve_approval_says_whether_one_waited() {
        let (test, id) = chat(Vec::new()).await;
        let request = Request::AgentResolveApproval {
            dialog_id: id,
            tool_call_id: "c".into(),
            decision: ApprovalDecision::Allow,
        };
        assert_eq!(call(&test.core, request).await, Response::Bool(false));
    }

    #[tokio::test]
    async fn agent_answer_questions_says_whether_they_waited() {
        let (test, id) = chat(Vec::new()).await;
        let request = Request::AgentAnswerQuestions {
            dialog_id: id,
            tool_call_id: "q".into(),
            outcome: QuestionsOutcome::Reply {
                reply: "Later".into(),
            },
        };
        assert_eq!(call(&test.core, request).await, Response::Bool(false));
    }

    #[tokio::test]
    async fn agent_status_is_the_runs_and_what_they_wait_on() {
        let test = TestCore::new();
        assert_eq!(
            call(&test.core, Request::AgentStatus).await,
            Response::AgentStatus(Default::default())
        );
    }

    #[tokio::test]
    async fn agent_configure_sets_the_settings_runs_follow() {
        let test = TestCore::new();
        let mut config = AgentConfig::default();
        config.permissions.allow_any_command = true;
        assert_eq!(
            call(
                &test.core,
                Request::AgentConfigure(Box::new(config.clone()))
            )
            .await,
            Response::Done
        );
        assert_eq!(test.core.agents.config(), config);
    }

    #[tokio::test]
    async fn agent_set_effort_counts_the_runs_it_reached() {
        let test = TestCore::new();
        let request = Request::AgentSetEffort {
            profile_id: "p::m".into(),
            options: None,
            dialog_id: None,
        };
        assert_eq!(call(&test.core, request).await, Response::Count(0));
    }

    fn todo_reply() -> NewMessage {
        NewMessage {
            tool_calls: Some(vec![ToolCall {
                id: "t".into(),
                name: "todo".into(),
                arguments: json!({ "items": ["Step one", "Step two"] }).to_string(),
                extra_content: None,
            }]),
            ..message("a", "assistant", "")
        }
    }

    #[tokio::test]
    async fn chat_checklist_is_the_chats_current_one() {
        let (test, id) = chat(vec![message("u", "user", "Go"), todo_reply()]).await;
        let Response::Checklist(Some(list)) =
            call(&test.core, Request::ChatChecklist { dialog_id: id }).await
        else {
            panic!("no checklist");
        };
        assert_eq!(list.items.len(), 2);
        assert_eq!(list.from_message_id.as_deref(), Some("a"));
    }

    #[tokio::test]
    async fn chat_dismiss_checklist_closes_it() {
        let (test, id) = chat(vec![message("u", "user", "Go"), todo_reply()]).await;
        let request = Request::ChatDismissChecklist {
            dialog_id: id,
            message_id: "a".into(),
        };
        assert_eq!(call(&test.core, request).await, Response::Checklist(None));
        assert_eq!(
            row(&test.core, "a").await.meta.unwrap()["checklistClosed"],
            true
        );
    }

    #[tokio::test]
    async fn workload_focus_is_done() {
        let test = TestCore::new();
        let request = Request::WorkloadFocus { dialog_id: None };
        assert_eq!(call(&test.core, request).await, Response::Done);
    }

    #[tokio::test]
    async fn chat_rename_renames_the_chat() {
        let (test, id) = chat(Vec::new()).await;
        let request = Request::ChatRename {
            dialog_id: id.clone(),
            title: "Renamed".into(),
        };
        assert_eq!(call(&test.core, request).await, Response::Done);
        let title = test
            .core
            .db
            .read(move |c| store::get_dialog(c, &id))
            .await
            .unwrap()
            .unwrap()
            .title;
        assert_eq!(title, "Renamed");
    }

    #[tokio::test]
    async fn chat_update_message_patches_its_row() {
        let (test, _) = chat(vec![message("u", "user", "Hi")]).await;
        let request = Request::ChatUpdateMessage {
            message_id: "u".into(),
            patch: MessagePatch {
                failed: Some(true),
                ..Default::default()
            },
        };
        assert_eq!(call(&test.core, request).await, Response::Done);
        assert!(row(&test.core, "u").await.failed);
    }

    #[tokio::test]
    async fn chat_set_model_keeps_the_chats_model() {
        let (test, id) = chat(Vec::new()).await;
        let model = ChatModel {
            provider_id: "p".into(),
            model_id: "m".into(),
            effort: Some("low".into()),
        };
        let request = Request::ChatSetModel {
            dialog_id: id.clone(),
            model: model.clone(),
        };
        assert_eq!(call(&test.core, request).await, Response::Done);
        let kept = test
            .core
            .db
            .read(move |c| store::chat_model(c, &id))
            .await
            .unwrap();
        assert_eq!(kept, Some(model));
    }

    #[tokio::test]
    async fn chat_delete_deletes_the_chat() {
        let (test, id) = chat(vec![message("u", "user", "Hi")]).await;
        let request = Request::ChatDelete {
            dialog_id: id.clone(),
        };
        assert_eq!(call(&test.core, request).await, Response::Done);
        let left = test
            .core
            .db
            .read(move |c| store::get_dialog(c, &id))
            .await
            .unwrap();
        assert!(left.is_none());
    }

    #[tokio::test]
    async fn clm_status_is_the_chats_context_state() {
        let (test, id) = chat(Vec::new()).await;
        let Response::ClmStatus(status) =
            call(&test.core, Request::ClmStatus { dialog_id: id }).await
        else {
            panic!("no status");
        };
        assert_eq!(status.mode, None);
        assert!(!status.available, "no virtual files in tests");
    }

    #[tokio::test]
    async fn clm_set_mode_sets_it_and_refuses_an_unknown_one() {
        let (test, id) = chat(Vec::new()).await;
        let request = Request::ClmSetMode {
            dialog_id: id.clone(),
            mode: "clm".into(),
        };
        assert_eq!(call(&test.core, request).await, Response::Done);
        let Response::ClmStatus(status) = call(
            &test.core,
            Request::ClmStatus {
                dialog_id: id.clone(),
            },
        )
        .await
        else {
            panic!("no status");
        };
        assert_eq!(status.mode.as_deref(), Some("clm"));
        let unknown = Request::ClmSetMode {
            dialog_id: id,
            mode: "other".into(),
        };
        let fail = dispatch(&test.core, unknown).await.unwrap_err();
        assert!(fail.message.contains("unknown context mode"), "{fail}");
    }

    #[tokio::test]
    async fn clm_reset_is_done() {
        let (test, id) = chat(Vec::new()).await;
        assert_eq!(
            call(&test.core, Request::ClmReset { dialog_id: id }).await,
            Response::Done
        );
    }

    #[tokio::test]
    async fn clm_list_revisions_lists_them() {
        let (test, id) = chat(Vec::new()).await;
        assert_eq!(
            call(&test.core, Request::ClmListRevisions { dialog_id: id }).await,
            Response::Revisions(Vec::new())
        );
    }

    #[tokio::test]
    async fn clm_read_revision_fails_for_one_that_isnt_there() {
        let (test, id) = chat(Vec::new()).await;
        let request = Request::ClmReadRevision {
            dialog_id: id,
            number: 3,
        };
        let fail = dispatch(&test.core, request).await.unwrap_err();
        assert_eq!(fail.code, FailCode::Internal);
        assert!(!fail.message.is_empty());
    }

    #[tokio::test]
    async fn host_info_is_the_daemons_to_answer() {
        let test = TestCore::new();
        let host = [
            Request::HostInfo,
            Request::KeysList,
            Request::McpSecretsKept,
            Request::Storage,
            Request::CleanUp,
            Request::PageOpened {
                id: "page-1".into(),
                local_port: Some(5173),
                error: None,
            },
            Request::AutonomyConfigure(Default::default()),
        ];
        for request in host {
            let fail = dispatch(&test.core, request).await.unwrap_err();
            assert_eq!(fail.code, FailCode::BadRequest);
        }
    }

    #[tokio::test]
    async fn clm_restore_revision_fails_for_one_that_isnt_there() {
        let (test, id) = chat(Vec::new()).await;
        let request = Request::ClmRestoreRevision {
            dialog_id: id,
            number: 3,
        };
        assert!(dispatch(&test.core, request).await.is_err());
    }

    /// A chat at `location`, with a message, in a fresh core.
    async fn located(location: Location) -> (TestCore, String) {
        let (test, id) = chat(vec![message("u", "user", "Hi")]).await;
        let at = id.clone();
        test.core
            .db
            .write(move |c| crate::chat_db::location::set(c, &at, &location))
            .await
            .unwrap();
        (test, id)
    }

    /// Each kind of request a chat can get: reads, run control, writes.
    fn requests(id: &str) -> (Vec<Request>, Vec<Request>, Vec<Request>) {
        let reads = vec![
            Request::ChatChecklist {
                dialog_id: id.into(),
            },
            Request::ClmStatus {
                dialog_id: id.into(),
            },
            Request::ClmListRevisions {
                dialog_id: id.into(),
            },
        ];
        let runs = vec![
            Request::AgentStop {
                dialog_id: id.into(),
            },
            Request::AgentPause {
                dialog_id: id.into(),
                host: "server".into(),
            },
            Request::AgentResolveApproval {
                dialog_id: id.into(),
                tool_call_id: "c".into(),
                decision: ApprovalDecision::Allow,
            },
        ];
        let writes = vec![
            Request::AgentStart(Box::new(start(id, "http://127.0.0.1:9", StartKind::Resume))),
            Request::ChatRename {
                dialog_id: id.into(),
                title: "Renamed".into(),
            },
            Request::ChatUpdateMessage {
                message_id: "u".into(),
                patch: MessagePatch {
                    content: Some("Changed".into()),
                    ..Default::default()
                },
            },
            Request::ChatSetModel {
                dialog_id: id.into(),
                model: ChatModel {
                    provider_id: "p".into(),
                    model_id: "m".into(),
                    effort: None,
                },
            },
            Request::ChatDismissChecklist {
                dialog_id: id.into(),
                message_id: "u".into(),
            },
            Request::ClmSetMode {
                dialog_id: id.into(),
                mode: "clm".into(),
            },
            Request::ChatDelete {
                dialog_id: id.into(),
            },
        ];
        (reads, runs, writes)
    }

    async fn refused(core: &Core, request: Request) -> String {
        let shown = format!("{request:?}");
        match dispatch(core, request).await {
            Err(fail) if fail.code == FailCode::NotOwner => fail.message,
            other => panic!("{shown} wasn't refused: {other:?}"),
        }
    }

    async fn served(core: &Core, request: Request) {
        let shown = format!("{request:?}");
        if let Err(fail) = dispatch(core, request).await {
            assert_ne!(fail.code, FailCode::NotOwner, "{shown}: {fail}");
        }
    }

    /// K21: a chat on a host changes there; its rows here are only read.
    #[tokio::test]
    async fn a_chat_on_a_host_is_only_read_here() {
        let (test, id) = located(Location::Host("server".into())).await;
        let (reads, runs, writes) = requests(&id);
        for request in runs.into_iter().chain(writes) {
            let why = refused(&test.core, request).await;
            assert!(why.contains("server"), "{why}");
        }
        for request in reads {
            served(&test.core, request).await;
        }
        let row = row(&test.core, "u").await;
        assert_eq!(row.content, "Hi", "nothing changed");
        let title = test
            .core
            .db
            .read(move |c| store::get_dialog(c, &id))
            .await
            .unwrap()
            .map(|d| d.title);
        assert_eq!(title.as_deref(), Some("Chat"));
    }

    /// K25: nothing changes a chat that is moving, and no run starts; the
    /// run that pauses for the move can still be stopped or answered.
    #[tokio::test]
    async fn a_chat_that_is_moving_takes_no_change_but_its_run_can_be_stopped() {
        let (test, id) = located(Location::Moving("server".into())).await;
        let (reads, runs, writes) = requests(&id);
        for request in writes {
            let why = refused(&test.core, request).await;
            assert!(why.contains("moving to server"), "{why}");
        }
        for request in reads.into_iter().chain(runs) {
            served(&test.core, request).await;
        }
    }

    /// On a daemon, a chat its computer took back is read, not changed.
    #[tokio::test]
    async fn a_released_chat_is_only_read() {
        let (test, id) = located(Location::Released).await;
        let (reads, runs, writes) = requests(&id);
        for request in runs.into_iter().chain(writes) {
            refused(&test.core, request).await;
        }
        for request in reads {
            served(&test.core, request).await;
        }
    }

    /// A chat owned here takes everything, as before.
    #[tokio::test]
    async fn a_chat_owned_here_takes_every_request() {
        let (test, id) = located(Location::Here).await;
        let (reads, runs, writes) = requests(&id);
        for request in reads.into_iter().chain(runs).chain(writes) {
            served(&test.core, request).await;
        }
    }

    /// The reply serializes as the frontend reads the command's result:
    /// the value, inside its variant.
    #[tokio::test]
    async fn a_reply_carries_the_commands_value() {
        let test = TestCore::new();
        let reply = serde_json::to_value(call(&test.core, Request::AgentStatus).await).unwrap();
        assert_eq!(reply["t"], "agentStatus");
        assert_eq!(
            reply["v"],
            json!({ "runs": [], "subagents": [], "approvals": [], "questions": [] })
        );
    }
}

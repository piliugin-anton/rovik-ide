//! What can be done to a chat, in one place (D3): the app's commands call
//! `dispatch` for a chat on this computer, and the daemon for every call it
//! gets, so the two serve the same operations the same way. The daemon adds
//! the Work and Host groups of section 4.2. The refusal of a chat owned
//! elsewhere (`NotOwner`) comes with the chats' location (M01).

use crate::agent_run;
use crate::chat_db::{api, attachments};
use crate::clm;
use crate::core::Core;
use crate::workload;
use rovik_protocol::{Fail, FailCode, Request, Response};

/// Serves a request of the Agent, Chat or CLM group. A failure carries the
/// error text the operation gives, which is what the app's command returns.
pub async fn dispatch(core: &Core, request: Request) -> Result<Response, Fail> {
    let (db, vfs) = (&core.db, &core.vfs);
    let done = |()| Response::Done;
    match request {
        Request::AgentStart(start) => agent_run::agent_start(core, start)
            .await
            .map(Response::RunId),
        Request::AgentStop { dialog_id } => {
            Ok(Response::Bool(agent_run::agent_stop(core, dialog_id)))
        }
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
        Request::AgentStatus => Ok(Response::AgentStatus(agent_run::agent_status(core))),
        Request::AgentConfigure(config) => {
            agent_run::agent_configure(core, config);
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
        Request::ProjectEnsure { .. }
        | Request::WorkSync { .. }
        | Request::WorkResult { .. }
        | Request::WorkResults
        | Request::HostInfo => {
            return Err(Fail::new(
                FailCode::BadRequest,
                "Only a daemon answers the Work and Host requests.",
            ))
        }
    }
    .map_err(|message| Fail::new(FailCode::Internal, message))
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
        let Response::RunId(Some(run_id)) =
            call(&test.core, Request::AgentStart(start(&id, &url, send))).await
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
            call(&test.core, Request::AgentStart(resume)).await,
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
        let fail = dispatch(&test.core, Request::AgentStart(send))
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
            call(&test.core, Request::AgentConfigure(config.clone())).await,
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
        let fail = dispatch(&test.core, Request::HostInfo).await.unwrap_err();
        assert_eq!(fail.code, FailCode::BadRequest);
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

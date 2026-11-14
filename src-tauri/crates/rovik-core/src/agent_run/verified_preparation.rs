//! The ordinary chat agent preparing a private, reviewable verification setup.
//!
//! A service-issued preparation owns the workspace and all source/plan bindings.
//! The model may propose inputs there; it cannot approve them or start implementation.

use super::{
    builtin_tools::BuiltinTool, messages, run::Run, tools::Outcome, StartKind, StartRequest,
};
use crate::{
    chat_db::store::{MessageRow, ToolCall},
    core::Core,
    restricted_files::Op,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

pub(super) const TOOL: &str = "prepare_verified_plan";
const MAX_TURNS: u32 = 80;
const MAX_TOOL_CALLS: u32 = 160;
const MAX_DURATION: Duration = Duration::from_secs(30 * 60);

/// Private authority resolved from the service, never from model tool arguments.
pub(super) struct Preparation {
    id: String,
    project_id: String,
    source_root: PathBuf,
    pub workspace_root: PathBuf,
    pub instructions: String,
    pub deadline: Instant,
    pub submitted: bool,
    turns: u32,
}

fn id_from_meta(meta: Option<&Value>) -> Result<Option<String>, String> {
    let Some(value) = meta.and_then(|meta| meta.get("verifiedPreparation")) else {
        return Ok(None);
    };
    let id = value
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 128)
        .ok_or("The verification preparation identity is invalid. Prepare the plan again.")?;
    Ok(Some(id.to_owned()))
}

/// Whether a send or durable continuation needs preparation support on its host.
/// This only inspects trusted chat metadata; workspace authority is resolved later.
pub async fn requires_preparation(core: &Core, request: &StartRequest) -> Result<bool, String> {
    Ok(id_from_meta(request_meta(core, request, None).await?.as_ref())?.is_some())
}

async fn request_meta(
    core: &Core,
    request: &StartRequest,
    recent: Option<&[MessageRow]>,
) -> Result<Option<Value>, String> {
    match &request.start {
        StartKind::Send { message } => Ok(message.meta.clone()),
        StartKind::Retry
        | StartKind::ContinueReply
        | StartKind::ContinueRun
        | StartKind::Resume => {
            if let Some(message) = recent.and_then(|rows| {
                rows.iter()
                    .rev()
                    .find(|message| messages::is_request(message))
            }) {
                return Ok(message.meta.clone());
            }
            let dialog = request.dialog_id.clone();
            core.db
                .read(move |db| {
                    let mut before = None;
                    loop {
                        let page = crate::chat_db::store::get_messages(db, &dialog, before, 500)?;
                        if let Some(message) = page
                            .messages
                            .iter()
                            .rev()
                            .find(|message| messages::is_request(message))
                        {
                            return Ok(message.meta.clone());
                        }
                        if !page.has_older || page.messages.is_empty() {
                            return Ok(None);
                        }
                        before = page.messages.first().map(|message| message.seq);
                    }
                })
                .await
        }
        _ => Ok(None),
    }
}

/// Resolve the current request's preparation for a new run or an explicit retry.
pub(super) async fn load(
    core: &Core,
    request: &StartRequest,
    recent: &[MessageRow],
    stop: &super::StopSignal,
) -> Result<Option<Preparation>, String> {
    let started = Instant::now();
    let meta = request_meta(core, request, Some(recent)).await?;
    let meta = meta.as_ref();
    let Some(id) = id_from_meta(meta)? else {
        return Ok(None);
    };
    if meta
        .and_then(|meta| meta.get("mode"))
        .and_then(Value::as_str)
        != Some("agent")
    {
        return Err("Verification preparation runs in Agent mode.".into());
    }
    if request.profile.no_tools || request.profile.connector_kind().is_some() {
        return Err("Choose a model with Rovik's tools to prepare this plan.".into());
    }
    let source_root = request
        .root
        .as_ref()
        .filter(|root| !root.trim().is_empty())
        .map(PathBuf::from)
        .ok_or("Open the plan's project before preparing verification.")?;
    let lookup = request.dialog_id.clone();
    let project_id = core
        .db
        .read(move |db| crate::chat_db::store::dialog_project(db, &lookup))
        .await?
        .ok_or("Select a project-backed chat before preparing verification.")?;
    let restrictions = crate::restricted_files::RestrictedFiles::load(
        &core.paths.config_dir,
        &source_root.to_string_lossy(),
    )
    .denied(&source_root);
    let state_root = core.paths.data_dir.join("harness");
    let context = service_call(
        crate::harness::ide::preparation_context(
            state_root.clone(),
            source_root.clone(),
            project_id.clone(),
            id.clone(),
            request.dialog_id.clone(),
            request.profile.clone(),
            restrictions,
        ),
        stop,
        started + MAX_DURATION,
        || {
            crate::harness::ide::cancel_active_preparation(
                &state_root,
                &source_root,
                &project_id,
                &id,
            )
        },
    )
    .await?;
    let now = rovik_harness::runner::now_ms().map_err(|error| error.to_string())?;
    let remaining = context
        .deadline_ms
        .checked_sub(now)
        .filter(|remaining| *remaining > 0)
        .ok_or("This preparation expired. Prepare the plan again.")?;
    let workspace_root = context.workspace_root;
    let config_dir = core.paths.config_dir.clone();
    let source = source_root.clone();
    let workspace = workspace_root.clone();
    crate::rt::spawn_blocking(move || hide_restricted_source(&config_dir, &source, &workspace))
        .await
        .map_err(|error| format!("Preparing private source failed: {error}"))??;
    Ok(Some(Preparation {
        id,
        project_id,
        source_root,
        workspace_root,
        instructions: context.instructions,
        deadline: (Instant::now() + Duration::from_millis(remaining)).min(started + MAX_DURATION),
        submitted: false,
        turns: 0,
    }))
}

/// Stop remains effective while the trusted service installs its runtime or
/// validates an oracle. Retry can reuse the preparation after cancellation.
async fn service_call<T>(
    operation: impl std::future::Future<Output = rovik_harness::Result<T>>,
    stop: &super::StopSignal,
    deadline: Instant,
    cancel: impl Fn() -> rovik_harness::Result<()>,
) -> Result<T, String> {
    tokio::pin!(operation);
    let mut poll = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            result = &mut operation => {
                if stop.is_set() { return Err("Verification preparation was stopped. It can be retried; implementation has not started.".into()); }
                return result.map_err(|error| error.to_string());
            }
            _ = poll.tick() => {
                if Instant::now() >= deadline { stop.set(); }
                if stop.is_set() { cancel().map_err(|error| error.to_string())?; }
            }
        }
    }
}

fn hide_restricted_source(
    config_dir: &std::path::Path,
    source: &std::path::Path,
    workspace: &std::path::Path,
) -> Result<(), String> {
    let restricted =
        crate::restricted_files::RestrictedFiles::load(config_dir, &source.to_string_lossy());
    // Remove copies before any repository map or prompt can inspect them. This
    // also prevents a later command from reading them through another filename.
    let mut removed = Vec::new();
    for entry in walkdir::WalkDir::new(workspace).follow_links(false) {
        let entry = entry.map_err(|error| format!("Inspecting private source failed: {error}"))?;
        if !entry.file_type().is_file() && !entry.file_type().is_symlink() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(workspace)
            .map_err(|error| error.to_string())?;
        let relative = relative.to_string_lossy().replace('\\', "/");
        if restricted.blocks_file(&relative, entry.path(), Op::Read) {
            removed.push(entry.into_path());
        }
    }
    for path in removed {
        std::fs::remove_file(&path)
            .map_err(|error| format!("Keeping restricted source private failed: {error}"))?;
    }
    Ok(())
}

/// Preparation has a smaller tool surface than implementation: no persistent
/// background work, MCP side effects, delegation, or delivery operations.
pub(super) fn permits(tool: BuiltinTool) -> bool {
    matches!(
        tool,
        BuiltinTool::Read
            | BuiltinTool::Write
            | BuiltinTool::ListDir
            | BuiltinTool::SearchCode
            | BuiltinTool::Neighbours
            | BuiltinTool::Callers
            | BuiltinTool::Outline
            | BuiltinTool::AskUser
            | BuiltinTool::Todo
            | BuiltinTool::RestoreContext
            | BuiltinTool::PrepareVerifiedPlan
    )
}

/// All paths resolve inside the issued workspace; only staged inputs are writable.
/// Resolving the deepest existing ancestor also rejects symlink escapes for new files.
pub(super) fn path_problem(
    root: &str,
    tool: BuiltinTool,
    args: &serde_json::Map<String, Value>,
) -> Option<String> {
    let path = args.get("path").and_then(Value::as_str)?;
    let relative = super::repair::real_key(root, path);
    let Some(relative) = relative.filter(|path| {
        !std::path::Path::new(path)
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    }) else {
        return Some("Preparation file tools can access only the issued private workspace.".into());
    };
    if tool == BuiltinTool::Write && !relative.starts_with(".rovik-preparation/") {
        return Some("Stage verification inputs under .rovik-preparation/. Source implementation is a separate step after review.".into());
    }
    None
}

/// Only the preparation lead is offered this closed, non-approving submission.
pub(super) fn schema() -> Value {
    json!({"type":"function","function":{
        "name":TOOL,
        "description":"Submit the prepared verification recipe for trusted validation and user review. This never approves inputs, starts implementation, applies changes, or claims verification. Fix reported preparation errors and submit again. All assets must be inside the issued preparation workspace.",
        "parameters":{"type":"object","additionalProperties":false,
            "properties":{"input_path":{"type":"string","description":"Relative path to the prepared JSON recipe inside this private workspace."}},
            "required":["input_path"]}
    }})
}

impl Preparation {
    #[cfg(test)]
    pub(super) fn for_test(source_root: PathBuf, workspace_root: PathBuf) -> Self {
        Self {
            id: "unissued-test-preparation".into(),
            project_id: "test-project".into(),
            source_root,
            workspace_root,
            instructions: "Prepare a recipe and independent oracle.".into(),
            deadline: Instant::now() + MAX_DURATION,
            submitted: false,
            turns: 0,
        }
    }

    pub(super) fn out_of_work(&self, tool_calls: u32) -> bool {
        tool_calls >= MAX_TOOL_CALLS || Instant::now() >= self.deadline
    }

    pub(super) fn next_turn(&mut self, tool_calls: u32) -> Result<(), String> {
        if self.turns >= MAX_TURNS || self.out_of_work(tool_calls) {
            return Err("Verification preparation reached its work limit. Review its progress before retrying; implementation has not started.".into());
        }
        self.turns += 1;
        Ok(())
    }
}

impl Run {
    pub(super) async fn submit_verified_preparation(
        &mut self,
        tc: &ToolCall,
        args: &serde_json::Map<String, Value>,
    ) -> Result<(), String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Submission {
            input_path: String,
        }
        let result: Result<Value, String> = async {
            let preparation = self
                .preparation
                .as_ref()
                .filter(|_| self.sub().is_none())
                .ok_or("This tool is available only while preparing a verified plan.")?;
            let input: Submission =
                serde_json::from_value(Value::Object(args.clone())).map_err(|error| {
                    format!("Provide only input_path for the prepared recipe: {error}")
                })?;
            if let Some(path) = self.restricted.blocked(
                &preparation.workspace_root.to_string_lossy(),
                &input.input_path,
                Op::Read,
            ) {
                return Err(crate::restricted_files::refusal(Op::Read, &path));
            }
            let state_root = self.core.paths.data_dir.join("harness");
            service_call(
                crate::harness::ide::submit_preparation(
                    state_root.clone(),
                    preparation.source_root.clone(),
                    preparation.project_id.clone(),
                    preparation.id.clone(),
                    self.dialog_id.clone(),
                    self.profile.clone(),
                    crate::harness::ide::PreparationSubmission {
                        input_path: input.input_path,
                        denied: self.restricted.denied(&preparation.source_root),
                    },
                ),
                &self.stop,
                preparation.deadline,
                || {
                    crate::harness::ide::cancel_active_preparation(
                        &state_root,
                        &preparation.source_root,
                        &preparation.project_id,
                        &preparation.id,
                    )
                },
            )
            .await
        }
        .await;
        let (ready, content, outcome) = submission_result(result);
        if ready {
            if let Some(preparation) = &mut self.preparation {
                preparation.submitted = true;
            }
        }
        self.add_tool_result(&tc.id, content, outcome, None, Vec::new())
            .await
    }
}

/// Only an explicit service-ready review completes preparation. Real trial
/// failures are valid service responses that the model must repair and resubmit.
fn submission_result(result: Result<Value, String>) -> (bool, String, Outcome) {
    match result {
        Ok(value) if value["status"] == "ready" && value["review"].is_object() => (
            true,
            format!("Preparation is ready for review. Implementation has not started.\n{value}"),
            Outcome::Done,
        ),
        Ok(value) => (
            false,
            format!("Verification preparation needs corrections. Read these trial diagnostics, repair the staged recipe or oracle, and submit again. Implementation has not started.\n{value}"),
            Outcome::Failed,
        ),
        Err(error) => (false, format!("Error: {error}"), Outcome::Failed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stopping_trusted_preparation_work_drains_it_before_returning() {
        let stop = super::super::StopSignal::new();
        let (send, receive) = tokio::sync::oneshot::channel::<()>();
        let send = std::sync::Mutex::new(Some(send));
        stop.set();
        let result = service_call(
            async {
                receive
                    .await
                    .map_err(|error| rovik_harness::Error::new(error.to_string()))?;
                Ok("cancelled work finished")
            },
            &stop,
            Instant::now() + MAX_DURATION,
            || {
                if let Some(send) = send.lock().unwrap().take() {
                    send.send(()).unwrap();
                }
                Ok(())
            },
        )
        .await;
        assert!(result.unwrap_err().contains("stopped"));
        assert!(send.lock().unwrap().is_none());
    }

    #[test]
    fn trial_failure_keeps_preparation_open_until_the_service_has_a_review() {
        let (ready, content, outcome) = submission_result(Ok(
            json!({"status":"preparing","trial":{"ready":false,"diagnostics":"expected test ID absent"}}),
        ));
        assert!(!ready);
        assert!(outcome.failed());
        assert!(content.contains("expected test ID absent"));
        assert!(!content.contains("ready for review"));
        assert!(!submission_result(Ok(json!({"status":"ready"}))).0);
        let (ready, _, outcome) =
            submission_result(Ok(json!({"status":"ready","review":{"digest":"trusted"}})));
        assert!(ready);
        assert!(!outcome.failed());
    }

    #[test]
    fn ordinary_requests_cannot_mint_a_preparation_identity() {
        assert_eq!(id_from_meta(None).unwrap(), None);
        assert_eq!(
            id_from_meta(Some(&json!({"implementsPlan":true}))).unwrap(),
            None
        );
        assert!(id_from_meta(Some(&json!({"verifiedPreparation":true}))).is_err());
        assert!(id_from_meta(Some(&json!({"verifiedPreparation":""}))).is_err());
        assert_eq!(
            id_from_meta(Some(&json!({"verifiedPreparation":"issued-id"}))).unwrap(),
            Some("issued-id".into())
        );
    }

    #[test]
    fn preparation_never_offers_external_or_persistent_work() {
        for tool in [
            BuiltinTool::Task,
            BuiltinTool::Mcp,
            BuiltinTool::Bash,
            BuiltinTool::Diagnostics,
            BuiltinTool::Background,
            BuiltinTool::SubmitPlan,
            BuiltinTool::MemoryRemember,
        ] {
            assert!(!permits(tool), "{tool:?}");
        }
        assert!(permits(BuiltinTool::AskUser));
        assert!(permits(BuiltinTool::PrepareVerifiedPlan));
    }

    #[test]
    fn preparation_paths_cannot_escape_or_write_implementation() {
        let temp = crate::testing::temp_dir();
        let root = temp.0.join("private");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(temp.0.join("outside"), "source").unwrap();
        let root = root.to_string_lossy();
        for path in [
            "../outside",
            "../../elsewhere",
            ".rovik-preparation/../../outside",
        ] {
            let args = json!({"path":path});
            assert!(
                path_problem(&root, BuiltinTool::Write, args.as_object().unwrap()).is_some(),
                "{path}"
            );
        }
        let source = json!({"path":"src/new.rs"});
        assert!(path_problem(&root, BuiltinTool::Write, source.as_object().unwrap()).is_some());
        let draft = json!({"path":".rovik-preparation/recipe.json"});
        assert!(path_problem(&root, BuiltinTool::Write, draft.as_object().unwrap()).is_none());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&temp.0, std::path::Path::new(root.as_ref()).join("escape"))
                .unwrap();
            let escaped = json!({"path":"escape/outside"});
            assert!(path_problem(&root, BuiltinTool::Read, escaped.as_object().unwrap()).is_some());
        }
    }

    #[test]
    fn preparation_limits_do_not_reset_with_tool_progress() {
        let mut preparation = Preparation::for_test(PathBuf::new(), PathBuf::new());
        for _ in 0..MAX_TURNS {
            assert!(preparation.next_turn(0).is_ok());
        }
        assert!(preparation.next_turn(0).is_err());
        let mut preparation = Preparation::for_test(PathBuf::new(), PathBuf::new());
        assert!(preparation.next_turn(MAX_TOOL_CALLS).is_err());
    }

    #[test]
    fn restricted_source_is_removed_only_from_the_private_copy() {
        let temp = crate::testing::temp_dir();
        let config = temp.0.join("config");
        let source = temp.0.join("source");
        let workspace = temp.0.join("private");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        for root in [&source, &workspace] {
            std::fs::write(root.join("private.txt"), "never send to model").unwrap();
            std::fs::write(root.join("code.rs"), "pub fn value() -> u8 { 1 }").unwrap();
        }
        crate::project_profile::save_project_settings(
            &config,
            source.to_string_lossy().into_owned(),
            json!({
                "restrictedFiles":[{"path":"private.txt","blockRead":true,"blockWrite":true}]
            }),
        )
        .unwrap();
        hide_restricted_source(&config, &source, &workspace).unwrap();
        assert!(source.join("private.txt").is_file());
        assert!(!workspace.join("private.txt").exists());
        assert!(workspace.join("code.rs").is_file());
    }
}

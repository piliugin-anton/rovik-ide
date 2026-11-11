//! Typed Git operations executed by the shared backend.

use super::{encode_result, DesktopResult};
use crate::desktop_session::Session;
use crate::git_sync::PullStrategy;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Git panel operations; paths and result shapes retain the native contract.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum GitRequest {
    /// `git_branches`.
    Branches { path: String },
    /// `git_head`.
    Head { path: String },
    /// `git_checkout`.
    Checkout { path: String, branch: String },
    /// `git_create_branch`.
    CreateBranch {
        path: String,
        name: String,
        checkout: bool,
        from: Option<String>,
    },
    /// `git_delete_branch`.
    DeleteBranch { path: String, name: String },
    /// `git_commit`.
    Commit {
        path: String,
        message: String,
        no_verify: Option<bool>,
    },
    /// `git_discard_all`.
    DiscardAll { path: String },
    /// `git_base_content`.
    BaseContent { path: String, file: String },
    /// `git_revert_file`.
    RevertFile { path: String, file: String },
    /// `git_revert_folder`.
    RevertFolder { path: String, folder: String },
    /// `git_log`.
    Log {
        path: String,
        limit: Option<u32>,
        query: Option<String>,
        offset: Option<usize>,
        tip: Option<String>,
    },
    /// `git_commit_show`.
    CommitShow {
        path: String,
        oid: String,
        query: Option<String>,
    },
    /// `git_commit_file_preview`.
    CommitFilePreview {
        path: String,
        oid: String,
        file: String,
    },
    /// `git_hooks`.
    Hooks { path: String },
    /// `git_hook_create`.
    HookCreate { path: String, name: String },
    /// `git_hook_make_executable`.
    HookMakeExecutable { path: String, name: String },
    /// `git_hook_delete`.
    HookDelete { path: String, name: String },
    /// `git_status`.
    Status { path: String },
    /// `git_init`.
    Init { path: String },
    /// `git_fetch`.
    Fetch { path: String },
    /// `git_push`.
    Push {
        path: String,
        no_verify: Option<bool>,
    },
    /// `git_stash_push`.
    StashPush {
        path: String,
        message: Option<String>,
    },
    /// `git_stash_list`.
    StashList { path: String },
    /// `git_stash_pop`.
    StashPop { path: String, index: Option<usize> },
    /// `git_stash_drop`.
    StashDrop { path: String, index: usize },
    /// `git_stash_show`.
    StashShow { path: String, index: usize },
    /// `git_stash_file_content`.
    StashFileContent {
        path: String,
        index: usize,
        file: String,
    },
    /// `git_stash_file_preview`.
    StashFilePreview {
        path: String,
        index: usize,
        file: String,
    },
    /// `git_pull_config`.
    PullConfig { path: String },
    /// `git_pull`.
    Pull {
        path: String,
        strategy: Option<PullStrategy>,
        remember: Option<bool>,
    },
    /// `git_rebase`.
    Rebase {
        path: String,
        onto: String,
        remote: bool,
        rewrite_pushed: Option<bool>,
    },
    /// `git_operation_continue`.
    OperationContinue {
        path: String,
        message: Option<String>,
        no_verify: Option<bool>,
    },
    /// `git_operation_abort`.
    OperationAbort { path: String },
    /// `git_conflict_file`.
    ConflictFile { path: String, file: String },
    /// `git_conflict_resolve`.
    ConflictResolve {
        path: String,
        file: String,
        content: Option<String>,
        side: Option<String>,
    },
}

impl GitRequest {
    pub(crate) async fn dispatch(self, session: Option<&Arc<Session>>) -> DesktopResult {
        let session = session.ok_or_else(|| {
            serde_json::Value::String("Git operations require a desktop session".into())
        })?;
        match self {
            Self::Branches { path } => encode_result(crate::git::git_branches(path).await),
            Self::Head { path } => encode_result(crate::git::git_head(path).await),
            Self::Checkout { path, branch } => {
                encode_result(crate::git::git_checkout(path, branch).await)
            }
            Self::CreateBranch {
                path,
                name,
                checkout,
                from,
            } => encode_result(crate::git::git_create_branch(path, name, checkout, from).await),
            Self::DeleteBranch { path, name } => {
                encode_result(crate::git::git_delete_branch(path, name).await)
            }
            Self::Commit {
                path,
                message,
                no_verify,
            } => encode_result(crate::git::git_commit(path, message, no_verify).await),
            Self::DiscardAll { path } => encode_result(crate::git::git_discard_all(path).await),
            Self::BaseContent { path, file } => {
                encode_result(crate::git::git_base_content(path, file).await)
            }
            Self::RevertFile { path, file } => {
                encode_result(crate::git::git_revert_file(path, file).await)
            }
            Self::RevertFolder { path, folder } => {
                encode_result(crate::git::git_revert_folder(path, folder).await)
            }
            Self::Log {
                path,
                limit,
                query,
                offset,
                tip,
            } => encode_result(
                crate::git::git_log(path, limit, query, offset, tip, session.git_history.clone())
                    .await,
            ),
            Self::CommitShow { path, oid, query } => {
                encode_result(crate::git::git_commit_show(path, oid, query).await)
            }
            Self::CommitFilePreview { path, oid, file } => {
                encode_result(crate::git::git_commit_file_preview(path, oid, file).await)
            }
            Self::Hooks { path } => encode_result(crate::git::git_hooks(path).await),
            Self::HookCreate { path, name } => {
                encode_result(crate::git::git_hook_create(path, name).await)
            }
            Self::HookMakeExecutable { path, name } => {
                encode_result(crate::git::git_hook_make_executable(path, name).await)
            }
            Self::HookDelete { path, name } => {
                encode_result(crate::git::git_hook_delete(path, name).await)
            }
            Self::Status { path } => encode_result(crate::git::git_status(path).await),
            Self::Init { path } => encode_result(crate::git::git_init(path).await),
            Self::Fetch { path } => encode_result(crate::git::git_fetch(path).await),
            Self::Push { path, no_verify } => {
                encode_result(crate::git::git_push(path, no_verify).await)
            }
            Self::StashPush { path, message } => {
                encode_result(crate::git::git_stash_push(path, message).await)
            }
            Self::StashList { path } => encode_result(crate::git::git_stash_list(path).await),
            Self::StashPop { path, index } => {
                encode_result(crate::git::git_stash_pop(path, index).await)
            }
            Self::StashDrop { path, index } => {
                encode_result(crate::git::git_stash_drop(path, index).await)
            }
            Self::StashShow { path, index } => {
                encode_result(crate::git::git_stash_show(path, index).await)
            }
            Self::StashFileContent { path, index, file } => {
                encode_result(crate::git::git_stash_file_content(path, index, file).await)
            }
            Self::StashFilePreview { path, index, file } => {
                encode_result(crate::git::git_stash_file_preview(path, index, file).await)
            }
            Self::PullConfig { path } => {
                encode_result(crate::git_sync::git_pull_config(path).await)
            }
            Self::Pull {
                path,
                strategy,
                remember,
            } => encode_result(crate::git_sync::git_pull(path, strategy, remember).await),
            Self::Rebase {
                path,
                onto,
                remote,
                rewrite_pushed,
            } => {
                encode_result(crate::git_sync::git_rebase(path, onto, remote, rewrite_pushed).await)
            }
            Self::OperationContinue {
                path,
                message,
                no_verify,
            } => encode_result(
                crate::git_sync::git_operation_continue(path, message, no_verify).await,
            ),
            Self::OperationAbort { path } => {
                encode_result(crate::git_sync::git_operation_abort(path).await)
            }
            Self::ConflictFile { path, file } => {
                encode_result(crate::git_sync::git_conflict_file(path, file).await)
            }
            Self::ConflictResolve {
                path,
                file,
                content,
                side,
            } => encode_result(
                crate::git_sync::git_conflict_resolve(path, file, content, side).await,
            ),
        }
    }
}

impl From<GitRequest> for super::DesktopRequest {
    fn from(request: GitRequest) -> Self {
        Self::Git(Box::new(request))
    }
}

//! Trusted source-baseline bindings for verification in synchronized chat worktrees.

use crate::{daemon::Daemon, work};
use rovik_core::{
    chat_db::{projects, store},
    harness::ide_source,
};
use rovik_protocol::harness::{Operation, SourceBinding};
use rovik_protocol::{Fail, FailCode, Response};
use std::path::PathBuf;

fn invalid(message: impl Into<String>) -> Fail {
    Fail::new(FailCode::BadRequest, message)
}

/// Resolve only the authenticated chat's current project and registered worktree.
async fn owned_context(daemon: &Daemon, dialog_id: &str) -> Result<(PathBuf, String), Fail> {
    let place = daemon
        .store
        .place(dialog_id)
        .ok_or_else(|| invalid("Synchronize this chat's project before using verification"))?;
    let root = work::root(daemon, dialog_id)
        .ok_or_else(|| invalid("This chat has no owned project worktree"))?;
    let id = dialog_id.to_string();
    let checked_root = root.clone();
    let project = place.project;
    let expected_project = project.clone();
    let authorized = daemon
        .core
        .db
        .read(move |db| {
            let actual = store::dialog_project(db, &id)?;
            Ok(actual.as_deref() == Some(expected_project.as_str())
                && projects::chat_may_use(db, actual.as_deref(), &checked_root)?)
        })
        .await
        .map_err(invalid)?;
    if !authorized {
        return Err(invalid(
            "The chat's project binding changed; reopen its execution context",
        ));
    }
    Ok((root, project))
}

/// Bind while the caller holds the worktree guard shared with synchronization.
async fn bind_current(
    daemon: &Daemon,
    dialog_id: &str,
    root: PathBuf,
    source: SourceBinding,
) -> Result<(), Fail> {
    if daemon.store.base_of(dialog_id).as_deref() != Some(source.snapshot.as_str()) {
        return Err(invalid(
            "The source binding no longer matches the chat's synchronized snapshot; inspect again",
        ));
    }
    let expected = source.snapshot.clone();
    let data_dir = daemon.core.paths.data_dir.clone();
    let id = dialog_id.to_string();
    rovik_core::rt::spawn_blocking(move || ide_source::bind(&data_dir, &id, &root, source))
        .await
        .map_err(|error| invalid(error.to_string()))?
        .map_err(|error| invalid(error.to_string()))?;
    if daemon.store.base_of(dialog_id).as_deref() != Some(expected.as_str()) {
        ide_source::invalidate(&daemon.core.paths.data_dir, dialog_id)
            .map_err(|error| invalid(error.to_string()))?;
        return Err(invalid(
            "The chat synchronized newer inputs while binding verification; inspect again",
        ));
    }
    Ok(())
}

/// Bind an authenticated strict Autopilot start before entering the core driver.
/// Release the file hold on return: the ordinary planning run acquires its own hold.
pub(crate) async fn bind_for_start(
    daemon: &Daemon,
    dialog_id: &str,
    source: SourceBinding,
) -> Result<(), Fail> {
    let _files = work::hold_files(daemon, dialog_id)?;
    let (root, _) = owned_context(daemon, dialog_id).await?;
    bind_current(daemon, dialog_id, root, source).await
}

/// Serve an already authenticated chat using only its owned project and worktree.
pub fn serve(
    daemon: &Daemon,
    dialog_id: String,
    source: Option<SourceBinding>,
    operation: Operation,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, Fail>> + Send + '_>> {
    Box::pin(async move {
        let fresh = operation.requires_current_source();
        // Private-job cancellation and exact approval decisions stay available.
        let _files = operation
            .requires_workspace_lock()
            .then(|| work::hold_files(daemon, &dialog_id))
            .transpose()?;
        let (root, project) = owned_context(daemon, &dialog_id).await?;
        let application = rovik_core::harness::ide::application_guard(
            daemon.core.paths.data_dir.join("harness"),
            root.clone(),
            project.clone(),
        )
        .await
        .map_err(|error| invalid(error.to_string()))?;
        if matches!(operation, Operation::ApplicationState) {
            return Ok(Response::Harness(application));
        }
        let applied = application["blocked"] == true;
        if applied && (source.is_some() || (fresh && !operation.observes_applied_workspace())) {
            return Err(invalid("Reviewed changes remain on this host. Desktop synchronization is blocked until their delivery or explicit reconciliation."));
        }
        if fresh && !applied && source.is_none() {
            return Err(invalid(
                "Synchronize the current desktop source before using verification",
            ));
        }
        if let Some(source) = source {
            bind_current(daemon, &dialog_id, root.clone(), source).await?;
        }
        let data_dir = daemon.core.paths.data_dir.clone();
        let id = dialog_id.clone();
        let bound_root = root.clone();
        let existing_context = !fresh || applied;
        let (reference, tracked_paths) = rovik_core::rt::spawn_blocking(move || {
            if existing_context {
                ide_source::resolve_existing(&data_dir, &id, &bound_root)
            } else {
                ide_source::resolve(&data_dir, &id, &bound_root)
            }
        })
        .await
        .map_err(|error| invalid(error.to_string()))?
        .map_err(|error| invalid(error.to_string()))?
        .ok_or_else(|| {
            invalid("Inspect and synchronize this chat's project before managing verification")
        })?;
        let result = rovik_core::harness::ide::execute_bound(
            daemon.core.paths.data_dir.join("harness"),
            root,
            project,
            Some(reference),
            Some(tracked_paths),
            operation,
        )
        .await
        .map(Response::Harness)
        .map_err(|error| invalid(error.to_string()));
        if result.is_ok() {
            rovik_core::agent_run::autopilot::reconcile_verified(&daemon.core, &dialog_id).await;
        }
        result
    })
}

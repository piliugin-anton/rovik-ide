//! Chats that move to the daemon and back (6.8).
//!
//! - **The import** (`POST /chats/import?move_id=…`): the bundle a computer
//!   sends, with the chat's rows since 0, its project and its snapshot. The
//!   worktree is made first, as a first sync makes one (T09), then the rows,
//!   the project's row and the worktree's folder go in in one transaction,
//!   and the chat's computer and move are recorded. The same move sent
//!   again answers "already imported". What an earlier stay left is
//!   cleared first (K36), and a chat another computer has here is refused.
//! - **`ChatOwner`**: whether the daemon has the chat for the computer that
//!   asks, and from which move, for a move cut short.
//! - **`ChatRelease`**: the computer took the chat back. Its rows are only
//!   read from then on, for the computer's last pull, until `ChatForget`.

use crate::daemon::Daemon;
use crate::store::{Moved, Place, ProjectState};
use crate::work;
use rovik_core::chat_db::location::{self, Location};
use rovik_core::chat_db::{api, projects, store, sync};
use rovik_protocol::sync::{ChatBundle, Imported};
use rovik_protocol::{Fail, FailCode, Response};

/// Imports the chat `computer` moves here with `move_id`.
pub async fn import(
    daemon: &Daemon,
    computer: &str,
    move_id: &str,
    bundle: ChatBundle,
) -> Result<Imported, Fail> {
    let chat = bundle.dialog_id.clone();
    if !work::plain(&chat) {
        return Err(bad_request(format!("{chat} isn't a chat's id.")));
    }
    if !work::plain(move_id) {
        return Err(bad_request("A move's id is letters, digits, - and _."));
    }
    sync::check_bundle(&chat, &bundle.rows).map_err(bad_request)?;
    let base = git2::Oid::from_str(&bundle.snapshot)
        .map_err(|_| bad_request("The snapshot isn't a commit's id."))?;
    let changes = work::commit_of(bundle.changes.clone())?;
    // One import at a time: the same move sent twice imports once.
    let _one = daemon.imports.lock().await;
    daemon.store.check_owner(computer, &chat)?;
    let owned = daemon.store.ownership(&chat);
    if let Some(owned) = owned.filter(|o| o.move_id.as_deref() == Some(move_id)) {
        return Ok(Imported {
            rev: owned.rev,
            already: true,
        });
    }
    if daemon.core.agents.is_running(&chat) {
        return Err(Fail::new(
            FailCode::Busy,
            "This chat's run is still going on the server.",
        ));
    }
    let project = bundle.project.clone();
    let excludes = bundle.excludes.clone();
    let repository =
        work::repository_with(daemon, &project.id, excludes.clone(), base, changes).await?;

    // What an earlier stay left: files first, its rows with the import.
    work::clear_stay(daemon, &chat, &project.id).await;
    api::remove_chat_and_children_files(&daemon.core, &chat)
        .await
        .map_err(internal)?;
    let state = ProjectState {
        name: project.name.clone(),
        subpath: project.subpath.clone(),
        settings: bundle.settings.clone(),
    };
    daemon
        .store
        .ensure_project(&project, bundle.settings.clone())
        .map_err(internal)?;
    let at = (project.id.as_str(), &state);
    let (worktree, root) = work::new_worktree(daemon, &chat, at, base, changes, excludes).await?;

    let (lead, rows, id, name, folder) = (
        chat.clone(),
        bundle.rows,
        project.id.clone(),
        project.name.clone(),
        root,
    );
    let rev = daemon
        .core
        .db
        .write(move |c| {
            let tx = c.transaction().map_err(|e| e.to_string())?;
            store::delete_subtree(&tx, &lead)?;
            projects::ensure_in(&tx, &id, &name, &repository)?;
            projects::ensure_in(&tx, &id, &name, &folder)?;
            let rev = sync::import_in(&tx, &lead, &rows, &id)?;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(rev)
        })
        .await
        .map_err(bad_request)?;
    let moved = Moved {
        move_id: move_id.to_string(),
        rev,
        place: Place {
            project: project.id.clone(),
            worktree,
        },
        base: bundle.snapshot,
        pending: bundle.pending,
    };
    daemon.store.imported(computer, &chat, moved)?;
    log::info!("{chat} moved here from {computer}");
    Ok(Imported {
        rev,
        already: false,
    })
}

/// `ChatOwner`: the chat as the daemon has it for the computer that asks.
pub fn owner(daemon: &Daemon, dialog_id: &str) -> Response {
    Response::Ownership(daemon.store.ownership(dialog_id))
}

/// `ChatRelease`: the chat goes back to its computer, if the move named
/// brought it. Released already, it stays so.
pub async fn release(
    daemon: &Daemon,
    dialog_id: &str,
    move_id: Option<String>,
) -> Result<Response, Fail> {
    // Never between an import's clearing and its rows.
    let _one = daemon.imports.lock().await;
    let Some(owned) = daemon.store.ownership(dialog_id) else {
        return Ok(Response::Bool(false));
    };
    if move_id.is_some() && owned.move_id != move_id {
        return Ok(Response::Bool(false));
    }
    if owned.released {
        return Ok(Response::Bool(true));
    }
    if daemon.core.agents.is_running(dialog_id) {
        return Err(Fail::new(
            FailCode::Busy,
            "This chat's run is still going on the server: pause it first.",
        ));
    }
    let id = dialog_id.to_string();
    daemon
        .core
        .db
        .write(move |c| location::set(c, &id, &Location::Released))
        .await
        .map_err(internal)?;
    daemon.store.release(dialog_id, store::now_ms());
    log::info!("{dialog_id} went back to its computer");
    Ok(Response::Bool(true))
}

fn bad_request(message: impl Into<String>) -> Fail {
    Fail::new(FailCode::BadRequest, message)
}

fn internal(message: String) -> Fail {
    Fail::new(FailCode::Internal, message)
}

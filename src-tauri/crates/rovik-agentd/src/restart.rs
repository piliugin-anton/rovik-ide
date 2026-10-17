//! Runs that a crash, a kill or the daemon's stop cut or paused (5.4): at
//! start-up, each chat whose run was going picks up where its stored rows
//! say, unless a crash cut it twice within the hour. An Autopilot task
//! (agent_run/autopilot.rs) goes on too: cut where no start picks its run
//! up, or between two of its stages, which no run's restart reaches.
//! Start-up's events reach the clients that subscribe from the start of
//! this boot.

use crate::daemon::Daemon;
use rovik_core::agent_run::autopilot;
use rovik_core::agent_run::events::AgentEvent;
use rovik_core::agent_run::recovery::{self, Recovery};
use rovik_core::agent_run::StartKind;
use rovik_core::chat_db::store::now_ms;
use rovik_protocol::Response;

/// The outcome of a run the daemon stopped where no start picks it up.
pub const INTERRUPTED: &str = "interrupted";

/// Each chat that was running when the daemon stopped, then each whose
/// Autopilot task was cut between two stages.
pub async fn restart_interrupted(daemon: &Daemon) {
    let running = daemon.store.running();
    for dialog_id in &running {
        restart(daemon, dialog_id).await;
    }
    for dialog_id in daemon.store.chats() {
        if running.contains(&dialog_id) || daemon.core.agents.is_running(&dialog_id) {
            continue;
        }
        if has_task(daemon, &dialog_id).await {
            resume_task(daemon, &dialog_id).await;
        }
    }
}

/// Whether the chat's Autopilot task isn't over (A03).
async fn has_task(daemon: &Daemon, dialog_id: &str) -> bool {
    autopilot::unfinished(&daemon.core, dialog_id)
        .await
        .unwrap_or_else(|e| {
            log::error!("chat {dialog_id}: its task's records couldn't be read: {e}");
            false
        })
}

/// Goes on with the chat's Autopilot task as a Resume does, counted as a
/// restart. False: it doesn't go on, and the chat waits.
async fn resume_task(daemon: &Daemon, dialog_id: &str) -> bool {
    if let Err(fail) = daemon.can_run(dialog_id) {
        log::warn!("chat {dialog_id}: its task isn't resumed: {fail}");
        return false;
    }
    if !counted(daemon, dialog_id) {
        return false;
    }
    let Some(request) = daemon.store.restart_request(dialog_id, StartKind::Resume) else {
        log::warn!("chat {dialog_id}: no settings to resume its task with");
        return false;
    };
    match daemon.run(request).await {
        Ok(_) => {
            log::info!("chat {dialog_id}: its task goes on");
            true
        }
        Err(fail) => {
            log::warn!("chat {dialog_id}: its task didn't go on: {fail}");
            false
        }
    }
}

/// Counts a restart of the chat's run (5.4): false, and the chat told,
/// when it was restarted twice within the hour already.
fn counted(daemon: &Daemon, dialog_id: &str) -> bool {
    if daemon.store.count_restart(dialog_id, now_ms()) {
        return true;
    }
    log::warn!("chat {dialog_id}: restarted twice within the hour, so not again");
    daemon.core.emit(AgentEvent::Notice {
        dialog_id: Some(dialog_id.to_string()),
        kind: "warning".into(),
        title: "Stopped after the server restarted twice".into(),
        message: "The server restarted twice within an hour while this chat's run \
                  was going, so it isn't started again. It waits for you."
            .into(),
    });
    false
}

async fn restart(daemon: &Daemon, dialog_id: &str) {
    let start = match recovery::after_crash(&daemon.core, dialog_id).await {
        Ok(Recovery::Start(start)) => start,
        Ok(Recovery::Paused) => return daemon.store.ended(dialog_id, None),
        // Cut where no start picks it up: an Autopilot task's run is
        // started again, as a Resume of its task does it.
        Ok(Recovery::Interrupted) => {
            if !(has_task(daemon, dialog_id).await && resume_task(daemon, dialog_id).await) {
                interrupted(daemon, dialog_id);
            }
            return;
        }
        Err(e) => {
            log::error!("chat {dialog_id}: its rows couldn't be read: {e}");
            return interrupted(daemon, dialog_id);
        }
    };
    if let Err(fail) = daemon.can_run(dialog_id) {
        log::warn!("chat {dialog_id}: not restarted: {fail}");
        return interrupted(daemon, dialog_id);
    }
    // A run the daemon's stop paused (R02) went down cleanly: it doesn't
    // count against the restarts a crashing run is allowed.
    let drained = start == StartKind::ContinueRun;
    if !drained && !counted(daemon, dialog_id) {
        return interrupted(daemon, dialog_id);
    }
    let Some(request) = daemon.store.restart_request(dialog_id, start.clone()) else {
        log::warn!("chat {dialog_id}: no settings to restart it with");
        return interrupted(daemon, dialog_id);
    };
    match daemon.run(request).await {
        Ok(Response::RunId(Some(run_id))) => {
            log::info!("chat {dialog_id}: restarted ({start:?}) as run {run_id}");
        }
        other => {
            log::warn!("chat {dialog_id}: {start:?} didn't start a run: {other:?}");
            interrupted(daemon, dialog_id);
        }
    }
}

/// The run ends where it was cut, as a run that ends does: the store and
/// the chat's computer hear of it, with outcome `interrupted`.
fn interrupted(daemon: &Daemon, dialog_id: &str) {
    daemon.core.emit(AgentEvent::RunEnded {
        dialog_id: dialog_id.to_string(),
        run_id: daemon.store.run_id(dialog_id).unwrap_or_default(),
        outcome: INTERRUPTED.into(),
        autopilot: false,
    });
}

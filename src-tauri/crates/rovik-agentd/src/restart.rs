//! Runs that a crash, a kill or an upgrade cut (5.4): at start-up, each
//! chat whose run was going picks up where its stored rows say, unless it
//! was restarted twice within the hour. Start-up's events reach the
//! clients that subscribe from the start of this boot.

use crate::daemon::Daemon;
use rovik_core::agent_run::events::AgentEvent;
use rovik_core::agent_run::recovery::{self, Recovery};
use rovik_core::chat_db::store::now_ms;
use rovik_protocol::Response;

/// The outcome of a run the daemon stopped where no start picks it up.
pub const INTERRUPTED: &str = "interrupted";

/// Each chat that was running when the daemon stopped.
pub async fn restart_interrupted(daemon: &Daemon) {
    for dialog_id in daemon.store.running() {
        restart(daemon, &dialog_id).await;
    }
}

async fn restart(daemon: &Daemon, dialog_id: &str) {
    let start = match recovery::after_crash(&daemon.core, dialog_id).await {
        Ok(Recovery::Start(start)) => start,
        Ok(Recovery::Paused) => return daemon.store.ended(dialog_id, None),
        Ok(Recovery::Interrupted) => return interrupted(daemon, dialog_id),
        Err(e) => {
            log::error!("chat {dialog_id}: its rows couldn't be read: {e}");
            return interrupted(daemon, dialog_id);
        }
    };
    if let Err(fail) = daemon.can_run() {
        log::warn!("chat {dialog_id}: not restarted: {fail}");
        return interrupted(daemon, dialog_id);
    }
    if !daemon.store.count_restart(dialog_id, now_ms()) {
        log::warn!("chat {dialog_id}: restarted twice within the hour, so not again");
        daemon.core.emit(AgentEvent::Notice {
            dialog_id: Some(dialog_id.to_string()),
            kind: "warning".into(),
            title: "Stopped after the server restarted twice".into(),
            message: "The server restarted twice within an hour while this chat's run \
                      was going, so it isn't started again. It waits for you."
                .into(),
        });
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
    });
}

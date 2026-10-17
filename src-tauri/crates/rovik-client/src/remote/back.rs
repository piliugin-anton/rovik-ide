//! Moving a chat back from its host (6.8), a step at a time, each kept in
//! the chat's `cloud_moves` row (`chat_db::moves`, K51):
//!
//! 1. The lock: the chat stays on its host, and the app refuses what would
//!    change it there. What its run waits on can still be answered.
//! 2. Its run pauses there (`AgentPause`, with no host: it comes back).
//! 3. Its worktree is committed (`WorkResult`), and the result is fetched
//!    and imported as the chat's store here, through the results' queue:
//!    waiting, never landed by itself or told to the host. This comes
//!    before the release, so a failure up to here leaves the chat on its
//!    host as it was.
//! 4. `ChatRelease`: the host refuses the chat's writes from now on, and the
//!    move can only be finished.
//! 5. A last pull, complete: nothing changes there any more.
//! 6. The flip: the chat is here, and its mirror's row goes. Its changes
//!    wait as a local isolated chat's do, or land first where its runs here
//!    couldn't see them (isolation off, or no view can be mounted).
//! 7. `ChatForget`, now or when the host is next connected, and the run
//!    goes on here with the start the app gave: `ContinueRun` after a pause
//!    for a move, `Resume` for tool calls without results.
//!
//! A move back cut short is settled once its host says whether it released
//! the chat (`ChatOwner`): finished from step 5 if it did, dropped if not,
//! the chat still there.

use super::results::Job;
use super::{internal, CloudEvent, Remote};
use crate::client::Client;
use rovik_core::agent_run::{self, StartKind, StartRequest};
use rovik_core::chat_db::location::{self, Location};
use rovik_core::chat_db::moves::{self, CloudMove};
use rovik_core::chat_db::store;
use rovik_core::overlay::chats;
use rovik_core::rt;
use rovik_protocol::sync::Ownership;
use rovik_protocol::{Fail, FailCode, Request};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::oneshot;

/// How long a move back waits for its run to pause: a command or an
/// approval can take long, and Stop ends the wait.
const PAUSE_WAIT: Duration = Duration::from_secs(24 * 60 * 60);

/// How long a deletion waits for its host before its tombstone waits for
/// the next connection: the host stops the chat's run first.
const TELL_WAIT: Duration = Duration::from_secs(30);

/// What a refused write to a chat moving back says.
pub(super) const MOVING_BACK: &str = "This chat is moving back to this computer.";

impl Remote {
    /// Moves the chat back from its host (6.8). `start` is how the app
    /// would start it: once it's here, its run picks up with it, if it
    /// paused for the move or waits for tool calls' results. Progress goes
    /// on `cloud-event` (`MovingBack`).
    pub async fn move_back(
        &self,
        dialog_id: &str,
        start: Option<StartRequest>,
    ) -> Result<(), Fail> {
        let host = self
            .host_of(dialog_id)
            .ok_or_else(|| refused("This chat is on this computer already."))?;
        let client = self.ready(&host)?;
        let root = self.returnable(dialog_id).await?;
        // Before its row, which a connection's settling then leaves alone.
        if !self.lock().moving.insert(dialog_id.to_string()) {
            return Err(refused(MOVING_BACK));
        }
        let move_id = uuid::Uuid::new_v4().to_string();
        let (chat, from, id) = (dialog_id.to_string(), host.clone(), move_id);
        let locked = self.core.db.write(move |c| {
            moves::lock_back(c, &chat, &from, &id)?;
            let mut chats = store::descendants(c, &chat)?;
            chats.push(chat);
            Ok(chats)
        });
        let chats = match locked.await {
            Ok(chats) => chats,
            Err(e) => {
                self.let_go(dialog_id);
                return Err(Fail::new(FailCode::BadRequest, e));
            }
        };
        {
            let mut state = self.lock();
            state.cancels.remove(dialog_id);
            for chat in chats {
                state.backs.insert(chat, dialog_id.to_string());
            }
        }
        // Released already when an earlier move back was cut short there.
        let owned = self.ownership(&client, dialog_id).await;
        let released = match owned {
            Ok(Some(owned)) => owned.released,
            // The host doesn't have the chat: what's here is all of it.
            Ok(None) => true,
            Err(fail) => return Err(self.give_up(&host, dialog_id, fail).await),
        };
        if !released {
            if let Err(fail) = self.fetch_back(&client, dialog_id, &host).await {
                return Err(self.give_up(&host, dialog_id, fail).await);
            }
            self.moving_back(dialog_id, "releasing", None);
            let release = Request::ChatRelease {
                dialog_id: dialog_id.to_string(),
                move_id: None,
            };
            if let Err(fail) = client.request::<bool>(release).await {
                return Err(self.give_up(&host, dialog_id, fail).await);
            }
        }
        self.finish_back(&client, dialog_id, &host, root, start)
            .await
    }

    /// Steps 2 and 3: the run paused there, and its last result here.
    async fn fetch_back(&self, client: &Client, dialog_id: &str, host: &str) -> Result<(), Fail> {
        if self.lock().running.contains_key(dialog_id) {
            self.moving_back(dialog_id, "pausing", None);
        }
        let pause = Request::AgentPause {
            dialog_id: dialog_id.to_string(),
            host: String::new(),
        };
        client.call_within(pause, PAUSE_WAIT).await?;
        self.step(dialog_id, moves::PAUSED).await?;
        self.not_cancelled(dialog_id)?;

        let id = dialog_id.to_string();
        let project = self.core.db.read(move |c| store::dialog_project(c, &id));
        if project.await.map_err(internal)?.is_some() {
            self.moving_back(dialog_id, "fetching", None);
            let (reply, taken) = oneshot::channel();
            let job = Job::Back {
                host: host.to_string(),
                dialog_id: dialog_id.to_string(),
                reply,
            };
            let closing = || Fail::new(FailCode::Internal, "The app is closing.");
            self.jobs.send(job).map_err(|_| closing())?;
            let taken = taken.await.map_err(|_| closing())?;
            taken.map_err(|e| Fail::new(FailCode::BadRequest, e))?;
        }
        self.step(dialog_id, moves::FETCHED).await?;
        self.not_cancelled(dialog_id)
    }

    /// Steps 5 to 7, once the host released the chat. A failure before the
    /// flip leaves the move at its release, to be finished when the host is
    /// next connected.
    async fn finish_back(
        &self,
        client: &Client,
        dialog_id: &str,
        host: &str,
        root: Option<PathBuf>,
        start: Option<StartRequest>,
    ) -> Result<(), Fail> {
        let flipped = self.pull_and_flip(dialog_id, host).await;
        self.let_go(dialog_id);
        if let Err(fail) = flipped {
            let message = format!(
                "{} It finishes once {} is connected again.",
                fail.message,
                self.host_name(host)
            );
            self.moving_back(dialog_id, "failed", Some(message.clone()));
            return Err(Fail::new(fail.code, message));
        }
        let goes_on = match root {
            Some(root) => self.settle_here(dialog_id, root, start.as_ref()).await,
            None => true,
        };
        self.changed(dialog_id);
        self.forget_there(client, dialog_id).await;
        self.moving_back(dialog_id, "moved", None);
        let Some(mut start) = start.filter(|_| goes_on) else {
            return Ok(());
        };
        let paused = agent_run::paused_for_move(&self.core, dialog_id).await;
        let waits = agent_run::waits_for_results(&self.core, dialog_id).await;
        start.start = match (paused, waits) {
            (Ok(true), _) => StartKind::ContinueRun,
            (_, Ok(true)) => StartKind::Resume,
            _ => return Ok(()),
        };
        start.dialog_id = dialog_id.to_string();
        if let Err(fail) = self.start(start).await {
            log::warn!("{dialog_id} moved back, but its run didn't pick up here: {fail}");
        }
        Ok(())
    }

    /// A move back that failed or was cancelled before the host released
    /// the chat: it stays there, as it was, and the results that came
    /// meanwhile, passed over for the move, are asked for again. One the
    /// host may have released, cut short there and taken up again, keeps
    /// its row, to be finished when the host is next connected.
    async fn give_up(&self, host: &str, dialog_id: &str, fail: Fail) -> Fail {
        let id = dialog_id.to_string();
        let dropped = self.core.db.write(move |c| match moves::of_chat(c, &id)? {
            Some(going) if going.step == moves::RELEASED => Ok(false),
            _ => moves::forget(c, &id).map(|()| true),
        });
        let dropped = dropped.await.unwrap_or_else(|e| {
            log::error!("{dialog_id}'s move back stays: {e}");
            false
        });
        self.let_go(dialog_id);
        if !dropped {
            let message = format!(
                "{} It finishes once {} is connected again.",
                fail.message,
                self.host_name(host)
            );
            self.moving_back(dialog_id, "failed", Some(message.clone()));
            return Fail::new(fail.code, message);
        }
        self.ask_results(host, Some(dialog_id)).await;
        let step = match fail.message.as_str() {
            super::moving::CANCELLED => "cancelled",
            _ => "failed",
        };
        self.moving_back(dialog_id, step, Some(fail.message.clone()));
        fail
    }

    /// Steps 5 and 6: the last pull, then the flip back, in the database
    /// and here: the chat and its subagents' chats are this computer's
    /// again.
    async fn pull_and_flip(&self, dialog_id: &str, host: &str) -> Result<(), Fail> {
        self.step(dialog_id, moves::RELEASED).await?;
        self.moving_back(dialog_id, "pulling", None);
        let pulled = self.pull_now(host, dialog_id).await;
        pulled
            .map_err(|e| Fail::new(FailCode::Unreachable, format!("Its last pull failed: {e}.")))?;
        let chat = dialog_id.to_string();
        let flipped = self.core.db.write(move |c| {
            let mut chats = store::descendants(c, &chat)?;
            moves::flip_back(c, &chat)?;
            chats.push(chat);
            Ok(chats)
        });
        let chats = flipped.await.map_err(internal)?;
        {
            let mut state = self.lock();
            for chat in &chats {
                state.chats.remove(chat);
                state.running.remove(chat);
                state.pulls.remove(chat);
                state.last_sync.remove(chat);
                state.syncs.remove(chat);
                state.rechecks.remove(chat);
            }
        }
        self.changed(dialog_id);
        Ok(())
    }

    /// The chat's changes, once it's here: they wait as a local isolated
    /// chat's do when its runs here can see them, in a view of their own.
    /// Otherwise they land first, or wait with their conflicts shown, and
    /// its run doesn't go on by itself. Whether it may.
    async fn settle_here(
        &self,
        dialog_id: &str,
        root: PathBuf,
        start: Option<&StartRequest>,
    ) -> bool {
        if !chats::exists(dialog_id) {
            return true;
        }
        let isolated = start.and_then(|s| s.isolate).unwrap_or_else(chats::enabled);
        if isolated {
            let chat = dialog_id.to_string();
            // The run opens the view again, with its own warnings.
            let viewed = rt::spawn_blocking(move || {
                let opened = chats::open(&chat, &root, Box::new(|_, _| {}));
                chats::close(&chat);
                opened.map(drop)
            });
            match viewed.await {
                Ok(Ok(())) => return true,
                Ok(Err(e)) => log::info!("{dialog_id}'s changes land first: {e}"),
                Err(e) => log::warn!("{dialog_id}'s changes land first: {e}"),
            }
        }
        self.moving_back(dialog_id, "landing", None);
        let chat = dialog_id.to_string();
        let landed = rt::spawn_blocking(move || chats::land_now(&chat, false)).await;
        let didnt = "This chat's changes didn't land";
        match landed {
            Ok(Ok(_)) => {
                let why = "Its runs here can't see changes of their own, so they went into the project first.";
                self.notice(dialog_id, "info", "This chat's changes landed", why);
                true
            }
            Ok(Err(e)) => {
                let message = format!(
                    "Its runs here can't see changes of their own, and these didn't land: {e}. Review them above the message box, then continue the chat."
                );
                self.notice(dialog_id, "warning", didnt, &message);
                false
            }
            Err(e) => {
                self.notice(dialog_id, "error", didnt, &e.to_string());
                false
            }
        }
    }

    /// `ChatForget`, the move back's last word: its row goes once the host
    /// heard it, and stays for the next connection if it didn't.
    pub(super) async fn forget_there(&self, client: &Client, dialog_id: &str) {
        let forget = Request::ChatForget {
            dialog_id: dialog_id.to_string(),
        };
        match client.request::<bool>(forget).await {
            Ok(_) => {}
            // Not this computer's there, or not released: nothing of this
            // move is left to tell it.
            Err(fail) if matches!(fail.code, FailCode::NotOwner | FailCode::BadRequest) => {
                log::warn!("{dialog_id}'s host didn't forget it: {fail}");
            }
            Err(fail) => {
                return log::warn!("{dialog_id}'s host forgets it later: {fail}");
            }
        }
        let id = dialog_id.to_string();
        if let Err(e) = self.core.db.write(move |c| moves::forget(c, &id)).await {
            log::warn!("{dialog_id}'s move back: {e}");
        }
    }

    /// A deleted chat's tombstone, told to its host (6.1): `ChatDelete`, or
    /// `ChatForget` for a chat it let go already. Its row goes once the host
    /// heard it, and stays for the next connection if it didn't.
    pub(super) async fn delete_there(&self, client: &Client, dialog_id: &str) {
        let id = dialog_id.to_string();
        let read = self.core.db.read(move |c| moves::of_chat(c, &id)).await;
        let step = match read {
            Ok(Some(going)) => going.step,
            Ok(None) => return,
            Err(e) => return log::warn!("{dialog_id}'s tombstone: {e}"),
        };
        if step != moves::DELETED {
            return self.forget_there(client, dialog_id).await;
        }
        let delete = Request::ChatDelete {
            dialog_id: dialog_id.to_string(),
        };
        match client.call_within(delete, TELL_WAIT).await {
            Ok(_) => {}
            // Let go there since: forgotten instead.
            Err(fail) if fail.code == FailCode::NotOwner => {
                return self.forget_there(client, dialog_id).await;
            }
            Err(fail) if fail.code == FailCode::BadRequest => {
                log::warn!("{dialog_id}'s host didn't delete it: {fail}");
            }
            Err(fail) => {
                return log::warn!("{dialog_id}'s host deletes it later: {fail}");
            }
        }
        let id = dialog_id.to_string();
        if let Err(e) = self.core.db.write(move |c| moves::forget(c, &id)).await {
            log::warn!("{dialog_id}'s tombstone: {e}");
        }
    }

    /// A move back the app left unfinished, once its host is connected: a
    /// queued `ChatForget` or a deleted chat's tombstone goes; one the host
    /// released is finished; one it didn't is dropped, the chat still there.
    pub(super) async fn finish_back_cut_short(
        &self,
        client: &Client,
        going: &CloudMove,
    ) -> Result<(), Fail> {
        let chat = going.dialog_id.as_str();
        if going.step == moves::FORGETTING {
            self.forget_there(client, chat).await;
            return Ok(());
        }
        if going.step == moves::DELETED {
            self.delete_there(client, chat).await;
            return Ok(());
        }
        if !self.lock().moving.insert(chat.to_string()) {
            return Ok(());
        }
        let settled = self.settle_cut_short(client, going).await;
        self.let_go(chat);
        settled
    }

    async fn settle_cut_short(&self, client: &Client, going: &CloudMove) -> Result<(), Fail> {
        let chat = going.dialog_id.as_str();
        let owned = self.ownership(client, chat).await?;
        let released = match &owned {
            Some(owned) => owned.released,
            None => true,
        };
        if !released {
            if going.step != moves::RELEASED {
                let id = chat.to_string();
                let dropped = self.core.db.write(move |c| moves::forget(c, &id));
                dropped.await.map_err(internal)?;
                self.moving_back(chat, "cancelled", None);
                return Ok(());
            }
            let release = Request::ChatRelease {
                dialog_id: chat.to_string(),
                move_id: None,
            };
            client.request::<bool>(release).await?;
        }
        let root = self.project_of(chat).await.ok().flatten();
        let root = root.map(|(_, root)| root);
        self.finish_back(client, chat, &going.host, root, None)
            .await
    }

    /// The chat can move back: a lead's chat, and its project's folder here
    /// to take its changes, if it has a project. That folder.
    async fn returnable(&self, dialog_id: &str) -> Result<Option<PathBuf>, Fail> {
        let id = dialog_id.to_string();
        let read = self.core.db.read(move |c| {
            let lead = store::lead_chat(c, &id)?;
            let at = location::of_chat(c, &id)?;
            Ok((lead, at, store::dialog_project(c, &id)?))
        });
        let (lead, at, project) = read.await.map_err(internal)?;
        if lead != dialog_id {
            return Err(refused("A subagent's chat moves with its lead's."));
        }
        if !matches!(at, Location::Host(_)) {
            return Err(refused("This chat isn't on a server."));
        }
        // Its mirror holds the task's records as the server has them.
        let candidates = agent_run::candidates::awaiting_pick(&self.core, dialog_id).await;
        if candidates.map_err(internal)? {
            return Err(refused(agent_run::candidates::MOVE_AWAITS_PICK));
        }
        if project.is_none() {
            return Ok(None);
        }
        let place = self.project_of(dialog_id).await.map_err(internal)?;
        let Some((_, root)) = place else {
            return Err(refused(
                "This chat's project has no folder on this computer: open it before moving the chat back.",
            ));
        };
        let chat = dialog_id.to_string();
        let conflicts =
            rt::spawn_blocking(move || chats::pending(&chat).map_or(0, |p| p.conflicts.len()));
        let conflicts = conflicts.await.unwrap_or(0);
        if conflicts > 0 {
            let files = if conflicts == 1 { "file" } else { "files" };
            return Err(refused(&format!(
                "{conflicts} {files} of this chat conflict with the project's changes: settle them first."
            )));
        }
        Ok(Some(root))
    }

    async fn ownership(&self, client: &Client, dialog_id: &str) -> Result<Option<Ownership>, Fail> {
        let asked = Request::ChatOwner {
            dialog_id: dialog_id.to_string(),
        };
        match client.request::<Option<Ownership>>(asked).await {
            Ok(owned) => Ok(owned),
            // Another computer's there: this one's copy is all it has.
            Err(fail) if fail.code == FailCode::NotOwner => Ok(None),
            Err(fail) => Err(fail),
        }
    }

    /// The chat's move back is over here, gone or not: what would change it
    /// goes through again, to wherever it is.
    fn let_go(&self, dialog_id: &str) {
        let mut state = self.lock();
        state.moving.remove(dialog_id);
        state.cancels.remove(dialog_id);
        state.backs.retain(|_, lead| lead != dialog_id);
    }

    /// Whether the chat is moving back, or its lead's: what would change it
    /// is refused.
    pub(super) fn is_moving_back(&self, dialog_id: &str) -> bool {
        self.lock().backs.contains_key(dialog_id)
    }

    fn moving_back(&self, dialog_id: &str, step: &str, message: Option<String>) {
        if let Some(cloud) = self.cloud() {
            cloud(CloudEvent::MovingBack {
                dialog_id: dialog_id.to_string(),
                step: step.to_string(),
                message,
            });
        }
    }
}

fn refused(why: &str) -> Fail {
    Fail::new(FailCode::BadRequest, why)
}

//! Remote chats' results, here (6.5). Each one is fetched into its
//! project's shadow and imported as the chat's overlay store, one at a time
//! in the order they came: from `ResultReady`, and from `WorkResults` after
//! every connection, so a result that ended while this computer was away
//! comes in too (K24). Results are numbered, and one no newer than the one
//! imported last, as a `ResultReady` replayed after a connection, or the
//! newest result again after the app restarts, can be, is passed over: the
//! chat's `mirror_state` keeps which it was. It lands by itself under
//! landing.rs's rules while the chat's project is the open project, and
//! waits as "Ready to land" otherwise. Each landing is reported to the host
//! with `WorkLanded`, at once or after its next connection, which
//! `mirror_state` remembers across a restart, so the next result doesn't
//! bring back what the user undid (K34).

use super::{takes_projects, Remote};
use crate::shadow::{self, Shadow};
use git2::Oid;
use rovik_core::agent_run::events::AgentEvent;
use rovik_core::chat_db::mirror::{self, MirrorState};
use rovik_core::chat_db::{projects, store};
use rovik_core::overlay::chats;
use rovik_core::overlay::import::{self, ImportError, Imported};
use rovik_core::overlay::land::{LandError, Landed};
use rovik_core::rt;
use rovik_protocol::work::{ChatResult, SyncState};
use rovik_protocol::Request;
use std::path::{Path, PathBuf};
use std::sync::Weak;
use tokio::sync::{mpsc, oneshot};

/// How many times a result goes back to be checked merged with the
/// project's newer changes before it waits (6.6).
const MAX_RECHECKS: u32 = 2;

/// What the queue of results does, one job after another.
pub(super) enum Job {
    /// A chat's result, from its host.
    Import { host: String, result: ChatResult },
    /// The open project changed: its results that waited for it land.
    Opened,
    /// The bar's Land; `keep_chat`: Land anyway.
    Land {
        dialog_id: String,
        keep_chat: bool,
        reply: oneshot::Sender<Result<Option<Landed>, LandError>>,
    },
}

/// The queue's task, until the app's `Remote` goes.
pub(super) async fn work_through(me: Weak<Remote>, mut jobs: mpsc::UnboundedReceiver<Job>) {
    while let Some(job) = jobs.recv().await {
        let Some(remote) = me.upgrade() else { break };
        remote.work(job).await;
    }
}

impl Remote {
    /// The project open in the app, by its id and folder: results land by
    /// themselves only there (6.5). Those that waited for it land now.
    pub fn set_open_project(&self, project: Option<(String, PathBuf)>) {
        self.lock().open = project;
        let _ = self.jobs.send(Job::Opened);
    }

    /// The user said what the folder at `root` is (a copy of another
    /// project, or one that moved), which made it `project`'s: when it's
    /// the open folder, results land there as that project's from now on.
    pub fn folder_answered(&self, root: &Path, project: &str) {
        let open = self.lock().open.clone();
        if open.is_some_and(|(id, at)| at == root && id != project) {
            self.set_open_project(Some((project.to_string(), root.to_path_buf())));
        }
    }

    /// Lands the chat's imported result as the bar's Land does
    /// (`keep_chat`: Land anyway), and tells its host.
    pub async fn land(
        &self,
        dialog_id: &str,
        keep_chat: bool,
    ) -> Result<Option<Landed>, LandError> {
        let (reply, landed) = oneshot::channel();
        let job = Job::Land {
            dialog_id: dialog_id.to_string(),
            keep_chat,
            reply,
        };
        let gone = || LandError::Failed("The app is closing.".into());
        self.jobs.send(job).map_err(|_| gone())?;
        landed.await.unwrap_or_else(|_| Err(gone()))
    }

    /// A result its host told of, into the queue.
    pub fn result_ready(&self, host: &str, result: ChatResult) {
        let host = host.to_string();
        let _ = self.jobs.send(Job::Import { host, result });
    }

    /// After each connection: the landings the host wasn't told of, then
    /// the newest result of each of this computer's chats there.
    pub(super) async fn after_connecting(&self, host: &str) {
        self.report_landings(host).await;
        self.ask_results(host, None).await;
    }

    /// The newest results on `host`, of every chat or of one, into the
    /// queue, which passes over those already imported.
    async fn ask_results(&self, host: &str, chat: Option<&str>) {
        let Some(client) = self.hosts.client(host).filter(takes_projects) else {
            return;
        };
        let results = client.request::<Vec<ChatResult>>(Request::WorkResults);
        match results.await {
            Ok(results) => {
                let wanted = results
                    .into_iter()
                    .filter(|r| chat.is_none_or(|chat| r.dialog_id == chat));
                for result in wanted {
                    self.result_ready(host, result);
                }
            }
            Err(fail) => log::warn!("{host}'s results: {fail}"),
        }
    }

    async fn work(&self, job: Job) {
        match job {
            Job::Import { host, result } => {
                let chat = result.dialog_id.clone();
                if let Err(e) = self.import(&host, result).await {
                    log::warn!("{chat}'s result didn't come back: {e}");
                    let message = format!("Its result couldn't be brought here: {e}");
                    self.notice(
                        &chat,
                        "error",
                        "This chat's changes didn't come back",
                        &message,
                    );
                }
                self.changed(&chat);
            }
            Job::Opened => self.land_waiting().await,
            Job::Land {
                dialog_id,
                keep_chat,
                reply,
            } => {
                let landed = self.land_imported(&dialog_id, Some(keep_chat)).await;
                let settled = landed.is_ok();
                let _ = reply.send(landed);
                // A newer result held back while the conflicts were
                // settled comes in now.
                if let Some(host) = self.host_of(&dialog_id).filter(|_| settled) {
                    self.ask_results(&host, Some(&dialog_id)).await;
                }
            }
        }
    }

    /// Fetches the chat's result and imports it, unless it came in before
    /// or a newer one did, then lands it if it may.
    async fn import(&self, host: &str, result: ChatResult) -> Result<(), String> {
        let chat = result.dialog_id.clone();
        if self.host_of(&chat).as_deref() != Some(host) {
            return Ok(());
        }
        let mirrored = self.mirror_state(&chat).await?.unwrap_or_default();
        let seen = mirrored.imported_seq;
        if passed_over(seen, result.seq) {
            return Ok(());
        }
        let (project, root) = self
            .project_of(&chat)
            .await?
            .ok_or("its project has no folder on this computer")?;
        let commit = Oid::from_str(&result.commit).map_err(|e| e.message().to_string())?;
        let dir = shadow::dir(&self.core.paths.data_dir, &project);
        let opened = {
            let dir = dir.clone();
            rt::spawn_blocking(move || Shadow::open(&dir, &root))
        };
        let shadow = opened.await.map_err(|e| e.to_string())??;
        if mirrored.imported_result.as_deref() == Some(result.commit.as_str()) {
            // The same changes, after a run that changed nothing more: its
            // outcome decides again whether they land, while they wait
            // here. A chat with no number kept can't tell, and they stay as
            // they are.
            let waits = chats::exists(&chat) && !import::resolving(&chat);
            if seen.is_some() && waits {
                self.decide(&chat, &project, result.pending.as_deref())
                    .await;
            }
            self.saw(&chat, &result).await;
            return Ok(());
        }
        let client = self.client(host).map_err(|fail| fail.message)?;
        let fetched = client.fetch(&project, &dir, &[commit]).await;
        fetched.map_err(|fail| fail.message)?;
        let told = result.clone();
        let imported = rt::spawn_blocking(move || shadow.import(&told));
        match imported.await.map_err(|e| e.to_string())? {
            Ok(imported) if imported.changed.is_empty() => {}
            Ok(_) => {
                self.decide(&chat, &project, result.pending.as_deref())
                    .await
            }
            Err(ImportError::Resolving) => {
                self.notice(
                    &chat,
                    "info",
                    "A newer result of this chat is ready",
                    "It comes in once its conflicts are settled.",
                );
                return Ok(());
            }
            Err(ImportError::Failed(e)) => return Err(e),
        }
        self.saw(&chat, &result).await;
        Ok(())
    }

    /// Lands the chat's imported result by itself, or has it wait: for
    /// the reason its run's outcome gives, or for its project to open.
    async fn decide(&self, dialog_id: &str, project: &str, pending: Option<&str>) {
        match pending {
            Some(reason) => import::wait(dialog_id, reason),
            None if !self.is_open(project) => import::wait(dialog_id, import::READY),
            None => {
                let _ = self.land_imported(dialog_id, None).await;
            }
        }
    }

    /// The chat's `result` came in: it and older ones are passed over from
    /// now on, after a restart too.
    async fn saw(&self, dialog_id: &str, result: &ChatResult) {
        let (id, commit, seq) = (dialog_id.to_string(), result.commit.clone(), result.seq);
        let kept = self
            .core
            .db
            .write(move |c| mirror::set_imported(c, &id, &commit, seq))
            .await;
        if let Err(e) = kept {
            log::warn!("{dialog_id}'s imported result isn't kept: {e}");
        }
    }

    async fn mirror_state(&self, dialog_id: &str) -> Result<Option<MirrorState>, String> {
        let id = dialog_id.to_string();
        self.core.db.read(move |c| mirror::state(c, &id)).await
    }

    /// The results that waited for their project, once it's open.
    async fn land_waiting(&self) {
        let Some((open, _)) = self.lock().open.clone() else {
            return;
        };
        let remote: Vec<String> = self.lock().chats.keys().cloned().collect();
        for chat in remote {
            let ready = chats::pending(&chat).is_some_and(|p| p.reason == import::READY);
            let project = self.project_of(&chat).await.ok().flatten();
            if ready && project.is_some_and(|(id, _)| id == open) {
                let _ = self.land_imported(&chat, None).await;
            }
        }
    }

    /// Lands the chat's imported result: by itself (`keep_chat` None), with
    /// a notice of what happened, or as the bar's Land does. A landing is
    /// reported to the chat's host.
    async fn land_imported(
        &self,
        dialog_id: &str,
        keep_chat: Option<bool>,
    ) -> Result<Option<Landed>, LandError> {
        let id = dialog_id.to_string();
        let landing = rt::spawn_blocking(move || {
            let imported = import::imported(&id);
            let landed = match keep_chat {
                None => import::land_by_itself(&id),
                Some(keep_chat) => chats::land_now(&id, keep_chat),
            };
            (imported, landed)
        });
        let (imported, landed) = landing
            .await
            .map_err(|e| LandError::Failed(e.to_string()))?;
        if let (Ok(_), Some(imported)) = (&landed, imported) {
            // The landing base moves there: the next message syncs again,
            // even with the project as it was at the last sync.
            {
                let mut state = self.lock();
                state.last_sync.remove(dialog_id);
                state.rechecks.remove(dialog_id);
            }
            self.report(dialog_id, imported).await;
        }
        match (&landed, keep_chat) {
            (Err(LandError::Unchecked(files)), None) => {
                self.recheck_or_wait(dialog_id, files).await
            }
            (_, None) => self.tell_landing(dialog_id, &landed),
            _ => {}
        }
        self.changed(dialog_id);
        landed
    }

    /// A result that would land merged with the project's newer changes
    /// goes back to its host, which merges them and runs the chat's checks
    /// on the merge (6.6). After two rounds of the project changing the
    /// same files again meanwhile, it waits.
    async fn recheck_or_wait(&self, dialog_id: &str, files: &[String]) {
        let rounds = self.lock().rechecks.get(dialog_id).copied().unwrap_or(0);
        if rounds >= MAX_RECHECKS {
            let again = LandError::Unchecked(files.to_vec());
            import::wait(
                dialog_id,
                &format!("Not landed: {again}. Land takes it as it is."),
            );
            let why = "The project changed the same files again while the checks ran.";
            self.notice(dialog_id, "warning", "This chat's changes didn't land", why);
            return;
        }
        self.lock()
            .rechecks
            .insert(dialog_id.to_string(), rounds + 1);
        let host = self.host_of(dialog_id).unwrap_or_default();
        let reason = match self.recheck(&host, dialog_id).await {
            Ok(synced) if synced.state == SyncState::Synced => format!(
                "Checking on the server: the project changed {} too.",
                files.join(", ")
            ),
            Ok(synced) => format!(
                "Not landed: the project changed {} again, on the chat's lines.",
                synced.files.join(", ")
            ),
            Err(e) => format!(
                "Not landed: the merge with the project's newer changes couldn't be checked on the server ({e})."
            ),
        };
        import::wait(dialog_id, &reason);
    }

    /// What a landing by itself did, as a local chat's run end says it.
    fn tell_landing(&self, dialog_id: &str, landed: &Result<Option<Landed>, LandError>) {
        let didnt = "This chat's changes didn't land";
        match landed {
            Ok(Some(landed)) => {
                let n = landed.paths.len();
                let message = format!(
                    "{n} change{} from this chat are in the project. Undo is above the message box.",
                    if n == 1 { "" } else { "s" }
                );
                self.notice(dialog_id, "success", "Landed", &message);
            }
            Ok(None) => {}
            Err(LandError::Conflicts(_)) => self.notice(
                dialog_id,
                "warning",
                didnt,
                "They conflict with changes in the project. Review them above the message box.",
            ),
            Err(LandError::Unchecked(_)) => self.notice(
                dialog_id,
                "warning",
                didnt,
                "The project changed some of the same files meanwhile, and the merged result isn't checked yet. Review it above the message box.",
            ),
            Err(e) => self.notice(dialog_id, "error", didnt, &e.to_string()),
        }
    }

    /// Tells the chat's host that `imported` landed, now or, when it can't
    /// be reached, after its next connection, which the chat's
    /// `mirror_state` keeps across a restart. A newer landing's report
    /// takes an older one's place: a result holds every change since the
    /// last landing it was told of.
    async fn report(&self, dialog_id: &str, imported: Imported) {
        let client = self.host_of(dialog_id).and_then(|h| self.hosts.client(&h));
        let landed = Request::WorkLanded {
            dialog_id: dialog_id.to_string(),
            commit: imported.commit.clone(),
            paths: imported.changed.clone(),
        };
        let told = match client {
            Some(client) => client.request::<()>(landed).await.map_err(|f| f.message),
            None => Err("its host isn't set up".into()),
        };
        let kept = match &told {
            Ok(()) => None,
            Err(e) => {
                log::warn!("{dialog_id}'s landing is told to its host later: {e}");
                serde_json::to_string(&imported).ok()
            }
        };
        let id = dialog_id.to_string();
        let written = self
            .core
            .db
            .write(move |c| mirror::set_unreported(c, &id, kept.as_deref()))
            .await;
        if let Err(e) = written {
            log::warn!("{dialog_id}'s landing to tell: {e}");
        }
    }

    async fn report_landings(&self, host: &str) {
        let at = host.to_string();
        let unreported = match self.core.db.read(move |c| mirror::unreported(c, &at)).await {
            Ok(unreported) => unreported,
            Err(e) => {
                log::warn!("the landings to tell {host}: {e}");
                return;
            }
        };
        for (chat, landing) in unreported {
            match serde_json::from_str::<Imported>(&landing) {
                Ok(imported) => self.report(&chat, imported).await,
                Err(e) => log::warn!("{chat}'s landing to tell can't be read: {e}"),
            }
        }
    }

    /// The chat's project here, by its id and folder: the open folder when
    /// it's the open project, else the folder it was last opened from.
    pub(super) async fn project_of(
        &self,
        dialog_id: &str,
    ) -> Result<Option<(String, PathBuf)>, String> {
        let id = dialog_id.to_string();
        let db = &self.core.db;
        let Some(project) = db.read(move |c| store::dialog_project(c, &id)).await? else {
            return Ok(None);
        };
        let open = self.lock().open.clone();
        if let Some((_, root)) = open.filter(|(id, _)| *id == project) {
            return Ok(Some((project, root)));
        }
        let id = project.clone();
        let summary = db.read(move |c| projects::summary(c, &id)).await?;
        Ok(summary
            .filter(|s| !s.missing)
            .map(|s| (project, PathBuf::from(s.path))))
    }

    fn is_open(&self, project: &str) -> bool {
        self.lock()
            .open
            .as_ref()
            .is_some_and(|(id, _)| id == project)
    }

    pub(super) fn notice(&self, dialog_id: &str, kind: &str, title: &str, message: &str) {
        self.core.emit(AgentEvent::Notice {
            dialog_id: Some(dialog_id.to_string()),
            kind: kind.into(),
            title: title.into(),
            message: message.into(),
        });
    }
}

/// Whether a result numbered `seq` is older than the newest one that came
/// in (`seen`), or that one again, as a replayed `ResultReady` can be. One
/// without a number (0) is judged by its commit alone.
fn passed_over(seen: Option<u64>, seq: u64) -> bool {
    seq > 0 && seen.is_some_and(|seen| seq <= seen)
}

#[cfg(test)]
mod tests {
    use super::passed_over;

    #[test]
    fn a_result_older_than_the_newest_is_passed_over() {
        assert!(!passed_over(None, 1), "the first of this session");
        assert!(!passed_over(Some(1), 2), "a newer one");
        assert!(passed_over(Some(2), 2), "the same one again");
        assert!(passed_over(Some(2), 1), "an older one, replayed");
        assert!(!passed_over(Some(2), 0), "unnumbered: by its commit");
    }
}

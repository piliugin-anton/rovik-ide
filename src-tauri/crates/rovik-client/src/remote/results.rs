//! Remote chats' results, here (6.5). Each one is fetched into its
//! project's shadow and imported as the chat's overlay store, one at a time
//! in the order they came: from `ResultReady`, and from `WorkResults` after
//! every connection, so a result that ended while this computer was away
//! comes in too (K24). Results are numbered, and one no newer than the one
//! imported last, as a `ResultReady` replayed after a connection, or the
//! newest result again after the app restarts, can be, is passed over: the
//! chat's `mirror_state` keeps which it was. It lands by itself under
//! landing.rs's rules while the chat's project is one the window has open,
//! and waits as "Ready to land" otherwise. Each landing is reported to the host
//! with `WorkLanded`, at once or after its next connection, which
//! `mirror_state` remembers across a restart, so the next result doesn't
//! bring back what the user undid (K34). A result that comes while the
//! user resolves the chat's conflicts is held back: the bar offers it in
//! their place, and it comes in by itself once they're settled.

use super::files::ChatFiles;
use super::{takes_projects, Remote};
use git2::Oid;
use rovik_core::agent_run::events::AgentEvent;
use rovik_core::chat_db::mirror::{self, MirrorState};
use rovik_core::overlay::chats;
use rovik_core::overlay::import::{self, ImportError, Imported};
use rovik_core::overlay::land::{LandError, Landed};
use rovik_core::rt;
use rovik_protocol::work::{ChatResult, SyncState};
use rovik_protocol::Request;
use std::path::PathBuf;
use std::sync::Weak;
use tokio::sync::{mpsc, oneshot};

/// How many times a result goes back to be checked merged with the
/// project's newer changes before it waits (6.6).
const MAX_RECHECKS: u32 = 2;

/// What the queue of results does, one job after another.
pub(super) enum Job {
    /// A chat's result, from its host.
    Import { host: String, result: ChatResult },
    /// A chat moving back: its last result, committed there and imported
    /// here to wait (remote/back.rs).
    Back {
        host: String,
        dialog_id: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// The projects open changed: the results that waited for one land.
    Opened,
    /// The bar's Land; `keep_chat`: Land anyway.
    Land {
        dialog_id: String,
        keep_chat: bool,
        reply: oneshot::Sender<Result<Option<Landed>, LandError>>,
    },
    /// The bar's Take the newer result.
    TakeNewer {
        dialog_id: String,
        reply: oneshot::Sender<Result<(), String>>,
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
    /// The projects open in the app's window, each by its id and folder:
    /// its folders', as the window's claims record them (the app's
    /// instances.rs, after each claim and each answer about a folder).
    /// Results land by themselves only in them (6.5); those that waited for
    /// one land now.
    pub fn set_open_projects(&self, open: Vec<(String, PathBuf)>) {
        self.lock().open = open;
        let _ = self.jobs.send(Job::Opened);
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

    /// Take the newer result: the chat's result held back while its
    /// conflicts were resolved comes in now, in their place, and what was
    /// resolved of them goes.
    pub async fn take_newer(&self, dialog_id: &str) -> Result<(), String> {
        self.check_here(dialog_id).await?;
        let (reply, taken) = oneshot::channel();
        let job = Job::TakeNewer {
            dialog_id: dialog_id.to_string(),
            reply,
        };
        let gone = || "The app is closing.".to_string();
        self.jobs.send(job).map_err(|_| gone())?;
        taken.await.unwrap_or_else(|_| Err(gone()))
    }

    /// Whether a newer result of the chat waits for its conflicts to be
    /// settled, for the bar.
    pub(super) fn newer_result(&self, dialog_id: &str) -> bool {
        self.lock().held.contains_key(dialog_id) && import::resolving(dialog_id)
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
    pub(super) async fn ask_results(&self, host: &str, chat: Option<&str>) {
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
            // Its move back brings its result.
            Job::Import { result, .. } if self.lock().moving.contains_key(&result.dialog_id) => {}
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
            Job::Back {
                host,
                dialog_id,
                reply,
            } => {
                let _ = reply.send(self.take_back(&host, &dialog_id).await);
                self.changed(&dialog_id);
            }
            Job::Opened => {
                self.land_waiting().await;
                // Results passed over as another window's while that one had
                // this project open: its window may have ended first.
                for (host, _) in self.hosts.ready() {
                    self.ask_results(&host, None).await;
                }
            }
            Job::Land {
                dialog_id, reply, ..
            } if self.is_moving_back(&dialog_id) => {
                let _ = reply.send(Err(LandError::Failed(super::back::MOVING_BACK.into())));
            }
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
            Job::TakeNewer {
                dialog_id, reply, ..
            } if self.is_moving_back(&dialog_id) => {
                let _ = reply.send(Err(super::back::MOVING_BACK.into()));
            }
            Job::TakeNewer { dialog_id, reply } => {
                let _ = reply.send(self.take_held(&dialog_id).await);
                self.changed(&dialog_id);
            }
        }
    }

    /// The chat's result held back while its conflicts were resolved,
    /// imported in their place. One that doesn't come in leaves them as
    /// they were.
    async fn take_held(&self, dialog_id: &str) -> Result<(), String> {
        let Some((host, result)) = self.lock().held.remove(dialog_id) else {
            return Ok(());
        };
        let pending = chats::pending(dialog_id);
        let before = import::imported(dialog_id).map(|i| i.commit);
        chats::set_pending(dialog_id, None);
        let imported = self.import(&host, result.clone()).await;
        if imported.is_err() {
            self.lock()
                .held
                .insert(dialog_id.to_string(), (host, result));
        }
        if import::imported(dialog_id).map(|i| i.commit) == before {
            chats::set_pending(dialog_id, pending.as_ref());
        }
        imported
    }

    /// Fetches the chat's result and imports it, unless it came in before
    /// or a newer one did, then lands it if it may.
    async fn import(&self, host: &str, result: ChatResult) -> Result<(), String> {
        let chat = result.dialog_id.clone();
        if self.host_of(&chat).as_deref() != Some(host) {
            return Ok(());
        }
        // Each of Rovik's processes is told of every result. The window
        // with the chat's project open imports it, and lands it there: one
        // without it would leave it waiting for a project open elsewhere.
        if self.handled_elsewhere(&chat).await {
            return Ok(());
        }
        // Its project open nowhere: the one that holds the chat first
        // imports it, and the others find it imported.
        let Some(_importing) = rovik_core::chat_hold::take(&chat) else {
            return Ok(());
        };
        let mirrored = self.mirror_state(&chat).await?.unwrap_or_default();
        let seen = mirrored.imported_seq;
        if passed_over(seen, result.seq) {
            return Ok(());
        }
        let files = self
            .files_of(&chat)
            .await?
            .ok_or("its project has no folder on this computer")?;
        let project = files.owner.clone();
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
        let (before, across) = (import::imported(&chat), files.across().is_some());
        let imported = self.fetch_and_import(host, files, &result).await?;
        match imported {
            Ok(imported) => {
                self.tell_left_there(host, &chat, before.as_ref(), &imported, across);
                if !imported.changed.is_empty() {
                    self.decide(&chat, &project, result.pending.as_deref())
                        .await
                }
            }
            Err(ImportError::Resolving) => {
                // The bar offers it in place of the conflicts.
                self.lock().held.insert(chat, (host.to_string(), result));
                return Ok(());
            }
            Err(ImportError::Failed(e)) => return Err(e),
        }
        self.lock().held.remove(&chat);
        self.saw(&chat, &result).await;
        Ok(())
    }

    /// The chat's `result`, fetched from its host into its shadow (made
    /// first if it isn't there, as after a snapshot), then imported as its
    /// store here (`ChatFiles::import`).
    async fn fetch_and_import(
        &self,
        host: &str,
        files: ChatFiles,
        result: &ChatResult,
    ) -> Result<Result<Imported, ImportError>, String> {
        let commit = Oid::from_str(&result.commit).map_err(|e| e.message().to_string())?;
        let data = self.core.paths.data_dir.clone();
        let dir = files.shadow(&data);
        let ready = {
            let data = data.clone();
            rt::spawn_blocking(move || files.ready(&data).map(|()| files))
        };
        let files = ready.await.map_err(|e| e.to_string())??;
        let client = self.client(host).map_err(|fail| fail.message)?;
        let fetched = client.fetch(&files.owner, &dir, &[commit]).await;
        fetched.map_err(|fail| fail.message)?;
        let told = result.clone();
        let imported = rt::spawn_blocking(move || files.import(&data, &told));
        imported.await.map_err(|e| e.to_string())
    }

    /// The chat's last result before it moves back (6.8): its worktree
    /// committed as it is, and imported as its store here unless it is
    /// already. It waits there, for the reason its run's outcome gives, or
    /// for the user: never landed by itself, nor told to its host, which is
    /// about to let the chat go.
    async fn take_back(&self, host: &str, dialog_id: &str) -> Result<(), String> {
        let client = self.client(host).map_err(|fail| fail.message)?;
        let ask = Request::WorkResult {
            dialog_id: dialog_id.to_string(),
        };
        let result: ChatResult = client.request(ask).await.map_err(|fail| fail.message)?;
        let files = self
            .files_of(dialog_id)
            .await?
            .ok_or("its project has no folder on this computer")?;
        let mirrored = self.mirror_state(dialog_id).await?.unwrap_or_default();
        if mirrored.imported_result.as_deref() != Some(result.commit.as_str()) {
            let (before, across) = (import::imported(dialog_id), files.across().is_some());
            let imported = self.fetch_and_import(host, files, &result).await?;
            let imported = imported.map_err(|e| e.to_string())?;
            self.tell_left_there(host, dialog_id, before.as_ref(), &imported, across);
            self.saw(dialog_id, &result).await;
        }
        if chats::exists(dialog_id) && !import::resolving(dialog_id) {
            let reason = result
                .pending
                .clone()
                .unwrap_or_else(|| format!("Ready to land: this chat moved back from {host}."));
            import::wait(dialog_id, &reason);
        }
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
        if self.lock().open.is_empty() {
            return;
        }
        let remote: Vec<String> = {
            let state = self.lock();
            let chats = state.chats.keys();
            chats
                .filter(|c| !state.backs.contains_key(*c))
                .cloned()
                .collect()
        };
        for chat in remote {
            let ready = chats::pending(&chat).is_some_and(|p| p.reason == import::READY);
            let files = self.files_of(&chat).await.ok().flatten();
            if ready && files.is_some_and(|files| self.is_open(&files.owner)) {
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
                self.notice(dialog_id, "success", "Landed", &landed.notice(""));
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

    /// Whether `project` is one the window has open: a project, or a
    /// workspace, whose folders the window lists under its ID too.
    pub(super) fn is_open(&self, project: &str) -> bool {
        self.lock().open.iter().any(|(id, _)| id == project)
    }

    /// Tells the user of what the chat's result `now` left on its host,
    /// unless the result imported `before` it left the same: the paths
    /// this computer takes for others that differ only in case (R04), and
    /// those of a chat `across` folders that are in none of its folders
    /// here (4.11), such as files its runs made at the top.
    fn tell_left_there(
        &self,
        host: &str,
        dialog_id: &str,
        before: Option<&Imported>,
        now: &Imported,
        across: bool,
    ) {
        let title = "Some of this chat's changes stay on the server";
        let host = self.host_name(host);
        let clashes = &now.case_clashes;
        if !clashes.is_empty() && before.is_none_or(|b| b.case_clashes != *clashes) {
            let message = case_clashes_notice(clashes, &host);
            self.notice(dialog_id, "warning", title, &message);
        }
        let outside = &now.outside;
        if across && !outside.is_empty() && before.is_none_or(|b| b.outside != *outside) {
            let message = outside_notice(outside, &host);
            self.notice(dialog_id, "warning", title, &message);
        }
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

/// The first few of `paths` by name, and how many more.
fn listed(paths: &[String]) -> String {
    let named: Vec<String> = paths.iter().take(3).map(|p| format!("`{p}`")).collect();
    match (named.as_slice(), paths.len()) {
        ([one], 1) => one.clone(),
        ([init @ .., last], n) if n == named.len() => format!("{} and {last}", init.join(", ")),
        (_, n) => format!("{} and {} more", named.join(", "), n - named.len()),
    }
}

/// What the notice of paths left on `host` for their case says: the first
/// few by name.
fn case_clashes_notice(paths: &[String], host: &str) -> String {
    let stay = if paths.len() == 1 { "stays" } else { "stay" };
    format!(
        "This computer can't tell apart names that differ only in case, so {} {stay} \
         on {host}; the chat's other changes come here.",
        listed(paths)
    )
}

/// What the notice of a chat's paths in none of its folders, left on
/// `host`, says: the first few by name.
fn outside_notice(paths: &[String], host: &str) -> String {
    let (are, stay) = match paths.len() {
        1 => ("is", "it stays"),
        _ => ("are", "they stay"),
    };
    format!(
        "{} {are} in none of the chat's folders, so {stay} on {host}; the chat's other \
         changes come here.",
        listed(paths)
    )
}

/// Whether a result numbered `seq` is older than the newest one that came
/// in (`seen`), or that one again, as a replayed `ResultReady` can be. One
/// without a number (0) is judged by its commit alone.
fn passed_over(seen: Option<u64>, seq: u64) -> bool {
    seq > 0 && seen.is_some_and(|seen| seq <= seen)
}

#[cfg(test)]
mod tests {
    use super::{case_clashes_notice, outside_notice, passed_over};

    #[test]
    fn the_paths_left_for_their_case_are_named() {
        let paths = |n: usize| -> Vec<String> { (1..=n).map(|i| format!("{i}.md")).collect() };
        assert_eq!(
            case_clashes_notice(&paths(1), "build box"),
            "This computer can't tell apart names that differ only in case, so `1.md` stays \
             on build box; the chat's other changes come here."
        );
        let named = |n: usize| {
            let notice = case_clashes_notice(&paths(n), "b");
            let (_, list) = notice.split_once(", so ").unwrap();
            list.split(" on b;").next().unwrap().to_string()
        };
        assert_eq!(named(2), "`1.md` and `2.md` stay");
        assert_eq!(named(3), "`1.md`, `2.md` and `3.md` stay");
        assert_eq!(named(5), "`1.md`, `2.md`, `3.md` and 2 more stay");
    }

    #[test]
    fn the_paths_in_none_of_a_chats_folders_are_named() {
        let one = ["notes.md".to_string()];
        assert_eq!(
            outside_notice(&one, "build box"),
            "`notes.md` is in none of the chat's folders, so it stays on build box; the \
             chat's other changes come here."
        );
        let two = ["a.md".to_string(), "b.md".to_string()];
        assert!(outside_notice(&two, "b").starts_with("`a.md` and `b.md` are in none"));
        assert!(outside_notice(&two, "b").contains("so they stay on b;"));
    }

    #[test]
    fn a_result_older_than_the_newest_is_passed_over() {
        assert!(!passed_over(None, 1), "the first of this session");
        assert!(!passed_over(Some(1), 2), "a newer one");
        assert!(passed_over(Some(2), 2), "the same one again");
        assert!(passed_over(Some(2), 1), "an older one, replayed");
        assert!(!passed_over(Some(2), 0), "unnumbered: by its commit");
    }
}

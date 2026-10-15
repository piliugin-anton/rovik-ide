//! Remote chats' results, here (6.5). Each one is fetched into its
//! project's shadow and imported as the chat's overlay store, one at a time
//! in the order they came: from `ResultReady`, and from `WorkResults` after
//! every connection, so a result that ended while this computer was away
//! comes in too (K24). It lands by itself under landing.rs's rules while
//! the chat's project is the open project, and waits as "Ready to land"
//! otherwise. Each landing is reported to the host with `WorkLanded`, at
//! once or after its next connection, so the next result doesn't bring
//! back what the user undid (K34).

use super::Remote;
use crate::shadow::{self, Shadow};
use git2::Oid;
use rovik_core::agent_run::events::AgentEvent;
use rovik_core::chat_db::{projects, store};
use rovik_core::overlay::chats;
use rovik_core::overlay::import::{self, ImportError, Imported};
use rovik_core::overlay::land::{LandError, Landed};
use rovik_core::rt;
use rovik_protocol::work::ChatResult;
use rovik_protocol::{features, Request};
use std::path::PathBuf;
use std::sync::Weak;
use tokio::sync::{mpsc, oneshot};

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

    /// A result the host told of.
    pub(super) fn result_ready(&self, host: &str, result: ChatResult) {
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
        let Some(client) = self.hosts.client(host) else {
            return;
        };
        let serves = |d: crate::DaemonInfo| d.features.iter().any(|f| f == features::PROJECTS);
        if !client.daemon().is_some_and(serves) {
            return;
        }
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

    /// Fetches the chat's result and imports it, unless it came in before,
    /// then lands it if it may.
    async fn import(&self, host: &str, result: ChatResult) -> Result<(), String> {
        let chat = result.dialog_id.clone();
        if self.host_of(&chat).as_deref() != Some(host) {
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
        if shadow.imported(&chat) == Some(commit) {
            return Ok(());
        }
        let client = self.client(host).map_err(|fail| fail.message)?;
        let fetched = client.fetch(&project, &dir, &[commit]).await;
        fetched.map_err(|fail| fail.message)?;
        let name = chat.clone();
        let imported = rt::spawn_blocking(move || shadow.import(&name, commit, &[]));
        match imported.await.map_err(|e| e.to_string())? {
            Ok(imported) if imported.changed.is_empty() => {}
            Ok(_) => match &result.pending {
                Some(reason) => import::wait(&chat, reason),
                None if !self.is_open(&project) => import::wait(&chat, import::READY),
                None => {
                    let _ = self.land_imported(&chat, None).await;
                }
            },
            Err(ImportError::Resolving) => self.notice(
                &chat,
                "info",
                "A newer result of this chat is ready",
                "It comes in once its conflicts are settled.",
            ),
            Err(ImportError::Failed(e)) => return Err(e),
        }
        Ok(())
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
            self.report(dialog_id, imported).await;
        }
        if keep_chat.is_none() {
            self.tell_landing(dialog_id, &landed);
        }
        landed
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
    /// be reached, after its next connection. A newer landing's report
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
        if let Err(e) = told {
            log::warn!("{dialog_id}'s landing is told to its host later: {e}");
            self.lock()
                .unreported
                .insert(dialog_id.to_string(), imported);
        }
    }

    async fn report_landings(&self, host: &str) {
        let unreported: Vec<(String, Imported)> = {
            let mut state = self.lock();
            let on_host: Vec<String> = state
                .unreported
                .keys()
                .filter(|chat| state.chats.get(*chat).is_some_and(|h| h == host))
                .cloned()
                .collect();
            on_host
                .into_iter()
                .filter_map(|chat| state.unreported.remove_entry(&chat))
                .collect()
        };
        for (chat, imported) in unreported {
            self.report(&chat, imported).await;
        }
    }

    /// The chat's project here, by its id and folder: the open folder when
    /// it's the open project, else the folder it was last opened from.
    async fn project_of(&self, dialog_id: &str) -> Result<Option<(String, PathBuf)>, String> {
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

    fn notice(&self, dialog_id: &str, kind: &str, title: &str, message: &str) {
        self.core.emit(AgentEvent::Notice {
            dialog_id: Some(dialog_id.to_string()),
            kind: kind.into(),
            title: title.into(),
            message: message.into(),
        });
    }
}

//! A remote chat's worktree brought up to date with the project here before
//! each message (6.7, D14): a snapshot of the project, pushed when it
//! changed since the chat's last sync, and `WorkSync`, which merges it on
//! the host. The message goes whatever the sync found. When the sync waits,
//! the chat goes on with the older copy and a notice says why. Each chat's
//! last sync is kept for the isolation bar.

use super::Remote;
use crate::client::Client;
use crate::shadow::{self, Shadow};
use rovik_core::chat_db::projects;
use rovik_core::{project_profile, rt};
use rovik_protocol::work::{ChatSync, Project, SyncState, Synced};
use rovik_protocol::{features, Request};
use std::path::Path;

impl Remote {
    /// Syncs the chat before a message to it. A sync that fails says so,
    /// and the message goes all the same, to the chat as it was.
    pub(super) async fn sync_before_send(&self, host: &str, dialog_id: &str) {
        if let Err(e) = self.sync(host, dialog_id).await {
            log::warn!("{dialog_id} didn't sync: {e}");
            let message = format!("The chat goes on with the project as it had it: {e}");
            self.notice(dialog_id, "warning", "The project didn't sync", &message);
        }
    }

    /// The chat's last sync, for the isolation bar.
    pub fn sync_state(&self, dialog_id: &str) -> Option<ChatSync> {
        self.lock().syncs.get(dialog_id).cloned()
    }

    /// A sync of the chat, as its host told it.
    pub(super) fn synced(&self, sync: ChatSync) {
        self.lock().syncs.insert(sync.dialog_id.clone(), sync);
    }

    async fn sync(&self, host: &str, dialog_id: &str) -> Result<(), String> {
        let client = self.client(host).map_err(|fail| fail.message)?;
        let serves = |d: crate::DaemonInfo| d.features.iter().any(|f| f == features::PROJECTS);
        // A chat without a project, or on a daemon without projects, works
        // in the folder its start names (--same-machine).
        let Some((project, root)) = self.project_of(dialog_id).await? else {
            return Ok(());
        };
        if !client.daemon().is_some_and(serves) {
            return Ok(());
        }
        let dir = shadow::dir(&self.core.paths.data_dir, &project);
        let (at, folder, chat) = (dir.clone(), root.clone(), dialog_id.to_string());
        let taken = rt::spawn_blocking(move || {
            let shadow = Shadow::open(&at, &folder)?;
            let snapshot = shadow.snapshot(&chat)?;
            Ok::<_, String>((
                snapshot.commit,
                shadow.excludes(),
                shadow.subpath().to_string(),
            ))
        });
        let (snapshot, excludes, subpath) = taken.await.map_err(|e| e.to_string())??;
        if self.lock().last_sync.get(dialog_id) == Some(&snapshot) {
            return Ok(());
        }
        self.ensure(host, &client, &project, &root, subpath, &excludes)
            .await?;
        let pushed = self.push_snapshot(dialog_id, &project, &dir, snapshot);
        pushed.await.map_err(|fail| fail.message)?;
        let sync = Request::WorkSync {
            dialog_id: dialog_id.to_string(),
            project,
            snapshot: snapshot.to_string(),
            excludes,
        };
        let synced: Synced = client.request(sync).await.map_err(|fail| fail.message)?;
        self.lock()
            .last_sync
            .insert(dialog_id.to_string(), snapshot);
        self.tell_sync(dialog_id, &synced);
        self.synced(ChatSync {
            dialog_id: dialog_id.to_string(),
            state: synced.state,
            files: synced.files,
        });
        Ok(())
    }

    /// The project on the host, with its name, its folder in its
    /// repository, its settings and the ignore rules only this computer
    /// has (K32): once a session.
    async fn ensure(
        &self,
        host: &str,
        client: &Client,
        project: &str,
        root: &Path,
        subpath: String,
        excludes: &[String],
    ) -> Result<(), String> {
        let key = (host.to_string(), project.to_string());
        if self.lock().ensured.contains(&key) {
            return Ok(());
        }
        let id = project.to_string();
        let summary = self.core.db.read(move |c| projects::summary(c, &id));
        let name = summary.await?.map(|s| s.name).unwrap_or_default();
        let config = &self.core.paths.config_dir;
        let settings = project_profile::load_project_settings(config, &root.to_string_lossy());
        let ensure = Request::ProjectEnsure {
            project: Project {
                id: project.to_string(),
                name,
                subpath,
            },
            settings: Some(settings).filter(|s| !s.is_null()),
            excludes: excludes.to_vec(),
        };
        client
            .request::<()>(ensure)
            .await
            .map_err(|fail| fail.message)?;
        self.lock().ensured.insert(key);
        Ok(())
    }

    /// A sync that waits says why: the chat goes on with the older copy.
    fn tell_sync(&self, dialog_id: &str, synced: &Synced) {
        let n = synced.files.len();
        let files = format!("{n} file{}", if n == 1 { "" } else { "s" });
        let why = match synced.state {
            SyncState::Synced | SyncState::Marked => return,
            SyncState::Conflicts => {
                format!("{files} changed both here and in the cloud chat, on the same lines.")
            }
            SyncState::Unmergeable => format!(
                "{files} changed both here and in the cloud chat can't be merged: binary, or deleted on one side."
            ),
        };
        let message = format!("{why} The chat goes on with the older copy.");
        self.notice(dialog_id, "warning", "Sync waits", &message);
    }
}

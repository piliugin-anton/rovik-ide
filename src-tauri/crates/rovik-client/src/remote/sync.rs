//! A remote chat's worktree brought up to date with the project here before
//! each message (6.7, D14): a snapshot of the project, pushed when it
//! changed since the chat's last sync, and `WorkSync`, which merges it on
//! the host. The message goes whatever the sync found. When the sync waits,
//! the chat goes on with the older copy and a notice says why. Each chat's
//! last sync is kept for the isolation bar.
//!
//! A result that would land merged with the project's newer changes goes
//! back the same way, with `WorkRecheck`: the host merges them and runs the
//! chat's checks on the merge (6.6).

use super::{takes_projects, Remote};
use crate::client::Client;
use crate::shadow::{self, Shadow};
use git2::Oid;
use rovik_core::chat_db::projects;
use rovik_core::{project_profile, rt};
use rovik_protocol::work::{ChatSync, Project, SyncState, Synced};
use rovik_protocol::Request;
use std::path::{Path, PathBuf};

/// A snapshot of a chat's project, and what goes to the host with it.
struct Snapshot {
    project: String,
    root: PathBuf,
    /// The project's shadow.
    shadow: PathBuf,
    commit: Oid,
    excludes: Vec<String>,
    subpath: String,
}

impl Remote {
    /// Syncs the chat before a message to it. A sync that fails says so,
    /// and the message goes all the same, to the chat as it was.
    pub(super) async fn sync_before_send(&self, host: &str, dialog_id: &str) {
        // A new message: its result gets its own rounds of rechecks.
        self.lock().rechecks.remove(dialog_id);
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

    /// Sends the project as it is now to the chat's host to merge into its
    /// worktree and check there (6.6).
    pub(super) async fn recheck(&self, host: &str, dialog_id: &str) -> Result<Synced, String> {
        let client = self.client(host).map_err(|fail| fail.message)?;
        let snapshot = self.snapshot(dialog_id).await?;
        let snapshot = snapshot.ok_or("its project has no folder on this computer")?;
        self.carry(host, &client, dialog_id, &snapshot).await?;
        let recheck = Request::WorkRecheck {
            dialog_id: dialog_id.to_string(),
            project: snapshot.project.clone(),
            snapshot: snapshot.commit.to_string(),
            excludes: snapshot.excludes.clone(),
        };
        let synced: Synced = client.request(recheck).await.map_err(|f| f.message)?;
        self.keep_sync(dialog_id, snapshot.commit, &synced);
        Ok(synced)
    }

    async fn sync(&self, host: &str, dialog_id: &str) -> Result<(), String> {
        let client = self.client(host).map_err(|fail| fail.message)?;
        // A chat without a project, or on a daemon without projects, works
        // in the folder its start names (--same-machine).
        if !takes_projects(&client) {
            return Ok(());
        }
        let Some(snapshot) = self.snapshot(dialog_id).await? else {
            return Ok(());
        };
        if self.lock().last_sync.get(dialog_id) == Some(&snapshot.commit) {
            return Ok(());
        }
        self.carry(host, &client, dialog_id, &snapshot).await?;
        let sync = Request::WorkSync {
            dialog_id: dialog_id.to_string(),
            project: snapshot.project.clone(),
            snapshot: snapshot.commit.to_string(),
            excludes: snapshot.excludes.clone(),
        };
        let synced: Synced = client.request(sync).await.map_err(|f| f.message)?;
        self.tell_sync(dialog_id, &synced);
        self.keep_sync(dialog_id, snapshot.commit, &synced);
        Ok(())
    }

    /// The chat's project as it is now, in its shadow; none when the chat
    /// has no project here.
    async fn snapshot(&self, dialog_id: &str) -> Result<Option<Snapshot>, String> {
        let Some((project, root)) = self.project_of(dialog_id).await? else {
            return Ok(None);
        };
        let shadow = shadow::dir(&self.core.paths.data_dir, &project);
        let (at, folder, chat) = (shadow.clone(), root.clone(), dialog_id.to_string());
        let taken = rt::spawn_blocking(move || {
            let shadow = Shadow::open(&at, &folder)?;
            let commit = shadow.snapshot(&chat)?.commit;
            let subpath = shadow.subpath().to_string();
            Ok::<_, String>((commit, shadow.excludes(), subpath))
        });
        let (commit, excludes, subpath) = taken.await.map_err(|e| e.to_string())??;
        Ok(Some(Snapshot {
            project,
            root,
            shadow,
            commit,
            excludes,
            subpath,
        }))
    }

    /// The snapshot on the host, and its project there.
    async fn carry(
        &self,
        host: &str,
        client: &Client,
        dialog_id: &str,
        snapshot: &Snapshot,
    ) -> Result<(), String> {
        self.ensure(host, client, snapshot).await?;
        let (project, shadow) = (&snapshot.project, &snapshot.shadow);
        let pushed = self.push_snapshot(dialog_id, project, shadow, snapshot.commit);
        pushed.await.map(drop).map_err(|fail| fail.message)
    }

    /// What a sync or a recheck did, kept: the snapshot it took, and its
    /// state for the bar.
    fn keep_sync(&self, dialog_id: &str, commit: Oid, synced: &Synced) {
        let mut state = self.lock();
        state.last_sync.insert(dialog_id.to_string(), commit);
        let sync = ChatSync {
            dialog_id: dialog_id.to_string(),
            state: synced.state,
            files: synced.files.clone(),
        };
        state.syncs.insert(dialog_id.to_string(), sync);
    }

    /// The project on the host, with its name, its folder in its
    /// repository, its settings and the ignore rules only this computer
    /// has (K32): once a session.
    async fn ensure(&self, host: &str, client: &Client, snapshot: &Snapshot) -> Result<(), String> {
        let key = (host.to_string(), snapshot.project.clone());
        if self.lock().ensured.contains(&key) {
            return Ok(());
        }
        let id = snapshot.project.clone();
        let summary = self.core.db.read(move |c| projects::summary(c, &id));
        let name = summary.await?.map(|s| s.name).unwrap_or_default();
        let settings = settings(&self.core.paths.config_dir, &snapshot.root);
        let ensure = Request::ProjectEnsure {
            project: Project {
                id: snapshot.project.clone(),
                name,
                subpath: snapshot.subpath.clone(),
            },
            settings,
            excludes: snapshot.excludes.clone(),
        };
        client.request::<()>(ensure).await.map_err(|f| f.message)?;
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

/// The project's settings here, when it has any.
fn settings(config_dir: &Path, root: &Path) -> Option<serde_json::Value> {
    let settings = project_profile::load_project_settings(config_dir, &root.to_string_lossy());
    Some(settings).filter(|s| !s.is_null())
}

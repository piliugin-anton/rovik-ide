//! A remote chat's worktree brought up to date with the project here before
//! each message (6.7, D14): a snapshot of the project, pushed when it
//! changed since the chat's last sync, and `WorkSync`, which merges it on
//! the host. The message goes whatever the sync found. When the sync waits,
//! the chat goes on with the older copy. Each chat's last sync is kept for
//! the isolation bar, which says why it waits and has the rest (U05): Sync
//! now, Hand to the chat (the text conflicts marked for the chat to settle)
//! and Keep the older copy (the bar stops asking until the next sync).
//!
//! A result that would land merged with the project's newer changes goes
//! back the same way, with `WorkRecheck`: the host merges them and runs the
//! chat's checks on the merge (6.6).

use super::{takes_projects, Remote};
use crate::client::Client;
use crate::shadow::{self, Shadow};
use git2::Oid;
use rovik_core::chat_db::projects;
use rovik_core::overlay::chats;
use rovik_core::{project_profile, rt};
use rovik_protocol::autopilot::{push_url, Delivery};
use rovik_protocol::work::{ChatSync, Project, SyncState, Synced};
use rovik_protocol::Request;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Why Sync now and Hand to the chat do nothing for a chat: it works in a
/// folder its start names, with no project of this computer's.
const NO_PROJECT: &str = "This chat has no project to sync.";

/// What Hand to the chat did: the chat's sync, and the message that asks
/// it to settle the markers it put in (empty: none went in).
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HandedSync {
    pub sync: ChatSync,
    pub request: String,
}

/// A snapshot of a chat's project, and what goes to the host with it.
pub(super) struct Snapshot {
    pub(super) project: String,
    pub(super) root: PathBuf,
    /// The project's shadow.
    pub(super) shadow: PathBuf,
    pub(super) commit: Oid,
    pub(super) excludes: Vec<String>,
    pub(super) subpath: String,
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

    /// The chat's last sync.
    pub fn sync_state(&self, dialog_id: &str) -> Option<ChatSync> {
        self.lock().syncs.get(dialog_id).cloned()
    }

    /// The chat's last sync as its isolation bar shows it: not once the
    /// user let it wait (Keep the older copy), until the next one.
    pub(super) fn bar_sync(&self, dialog_id: &str) -> Option<ChatSync> {
        let state = self.lock();
        let kept = state.kept.contains(dialog_id);
        state.syncs.get(dialog_id).filter(|_| !kept).cloned()
    }

    /// A sync of the chat, as its host told it.
    pub(super) fn synced(&self, sync: ChatSync) {
        let chat = sync.dialog_id.clone();
        {
            let mut state = self.lock();
            // The event of the sync the bar shows, come late, leaves what
            // the user chose about it.
            if state.syncs.get(&chat) != Some(&sync) {
                state.kept.remove(&chat);
            }
            state.syncs.insert(chat.clone(), sync);
        }
        self.changed(&chat);
    }

    /// Sync now: the project as it is here, merged into the chat's
    /// worktree on its host as before a message. None when the chat has it
    /// already: its last sync took it, and applied.
    pub async fn sync_now(&self, dialog_id: &str) -> Result<Option<ChatSync>, String> {
        let (host, client, snapshot) = self.to_sync(dialog_id).await?;
        let had = {
            let state = self.lock();
            let applied = state.syncs.get(dialog_id).is_some_and(|s| applied(s.state));
            applied && state.last_sync.get(dialog_id) == Some(&snapshot.commit)
        };
        if had {
            return Ok(None);
        }
        let sync = self.sync_with(&host, &client, dialog_id, &snapshot, false);
        sync.await.map(Some)
    }

    /// Hand to the chat: the project as it is here, merged into the chat's
    /// worktree with markers in the files that conflict on the same lines,
    /// whatever its setting, and the message that asks it to settle them.
    /// A sync that applies without conflicts now, or waits for files that
    /// can't take markers, has no message.
    pub async fn sync_to_chat(&self, dialog_id: &str) -> Result<HandedSync, String> {
        let (host, client, snapshot) = self.to_sync(dialog_id).await?;
        let sync = self
            .sync_with(&host, &client, dialog_id, &snapshot, true)
            .await?;
        let request = match sync.state {
            SyncState::Marked => chats::resolve_request(&sync.files),
            _ => String::new(),
        };
        Ok(HandedSync { sync, request })
    }

    /// Keep the older copy: the chat goes on without the project's changes
    /// its sync waits with, and its bar stops asking until the next sync.
    /// Its landing base stays, so landing its result brings the conflicts
    /// up here.
    pub fn keep_older_copy(&self, dialog_id: &str) {
        {
            let mut state = self.lock();
            let waits = state
                .syncs
                .get(dialog_id)
                .is_some_and(|s| !applied(s.state));
            if waits {
                state.kept.insert(dialog_id.to_string());
            }
        }
        self.changed(dialog_id);
    }

    /// What a sync the bar asks for takes: the chat's host, its client,
    /// and the project's snapshot now.
    async fn to_sync(&self, dialog_id: &str) -> Result<(String, Client, Snapshot), String> {
        let host = self
            .host_of(dialog_id)
            .ok_or("This chat isn't on a host.")?;
        let client = self.client(&host).map_err(|fail| fail.message)?;
        if !takes_projects(&client) {
            return Err(NO_PROJECT.into());
        }
        let snapshot = self.snapshot(dialog_id).await?.ok_or(NO_PROJECT)?;
        Ok((host, client, snapshot))
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
        let sync = self.sync_with(host, &client, dialog_id, &snapshot, false);
        sync.await.map(drop)
    }

    /// Carries the snapshot to the chat's host and merges it into the
    /// chat's worktree there (`mark`: the text conflicts marked whatever
    /// the chat's setting), and keeps what that did.
    async fn sync_with(
        &self,
        host: &str,
        client: &Client,
        dialog_id: &str,
        snapshot: &Snapshot,
        mark: bool,
    ) -> Result<ChatSync, String> {
        self.carry(host, client, dialog_id, snapshot).await?;
        let sync = Request::WorkSync {
            dialog_id: dialog_id.to_string(),
            project: snapshot.project.clone(),
            snapshot: snapshot.commit.to_string(),
            excludes: snapshot.excludes.clone(),
            changes: None,
            mark,
        };
        let synced: Synced = client.request(sync).await.map_err(|f| f.message)?;
        Ok(self.keep_sync(dialog_id, snapshot.commit, &synced))
    }

    /// The chat's project as it is now, in its shadow; none when the chat
    /// has no project here.
    pub(super) async fn snapshot(&self, dialog_id: &str) -> Result<Option<Snapshot>, String> {
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
    pub(super) async fn carry(
        &self,
        host: &str,
        client: &Client,
        dialog_id: &str,
        snapshot: &Snapshot,
    ) -> Result<(), String> {
        self.ensure(host, client, snapshot).await?;
        let (project, shadow) = (&snapshot.project, &snapshot.shadow);
        let pushed = self.push_to(host, dialog_id, project, shadow, snapshot.commit);
        pushed.await.map(drop).map_err(|fail| fail.message)
    }

    /// What a sync or a recheck did, kept: the snapshot it took, and its
    /// state for the bar.
    fn keep_sync(&self, dialog_id: &str, commit: Oid, synced: &Synced) -> ChatSync {
        let sync = ChatSync {
            dialog_id: dialog_id.to_string(),
            state: synced.state,
            files: synced.files.clone(),
        };
        {
            let mut state = self.lock();
            state.last_sync.insert(dialog_id.to_string(), commit);
            state.kept.remove(dialog_id);
            state.syncs.insert(dialog_id.to_string(), sync.clone());
        }
        self.changed(dialog_id);
        sync
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
        // A host that pushes its results (D15, H04) is told where: the
        // project's `origin`, as an SSH address without credentials.
        let remote = match self.autonomy_config(host).delivery {
            Delivery::PushBranch { .. } => origin_of(&snapshot.root).as_deref().and_then(push_url),
            Delivery::KeepOnHost => None,
        };
        let ensure = Request::ProjectEnsure {
            project: Project {
                id: snapshot.project.clone(),
                name,
                subpath: snapshot.subpath.clone(),
            },
            settings,
            excludes: snapshot.excludes.clone(),
            remote,
        };
        client.request::<()>(ensure).await.map_err(|f| f.message)?;
        self.lock().ensured.insert(key);
        Ok(())
    }
}

/// Whether a sync in `state` brought the project's changes: one that waits
/// left the chat with the older copy.
fn applied(state: SyncState) -> bool {
    matches!(state, SyncState::Synced | SyncState::Marked)
}

/// The URL of the `origin` remote of the repository `root` is in, if any.
fn origin_of(root: &Path) -> Option<String> {
    let repo = git2::Repository::discover(root).ok()?;
    let origin = repo.find_remote("origin").ok()?;
    origin.url().ok().map(str::to_string)
}

/// The project's settings here, when it has any.
pub(super) fn settings(config_dir: &Path, root: &Path) -> Option<serde_json::Value> {
    let settings = project_profile::load_project_settings(config_dir, &root.to_string_lossy());
    Some(settings).filter(|s| !s.is_null())
}

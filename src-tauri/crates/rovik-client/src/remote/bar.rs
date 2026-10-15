//! The isolation bar of a remote chat (6.1): its status, from the imported
//! result and from the files the chat changed in its worktree so far, with
//! its last sync; a changed file's versions; and what the bar's buttons do.
//! Landing, Undo and the conflict resolver work on the imported result
//! here, but "Is the run going?" is asked of the chat's host. The buttons
//! that undo a local merge or a group's steps aren't there in v1.

use super::Remote;
use rovik_core::agent_run::AgentStatus;
use rovik_core::overlay::chats::{self, ChangedFile, FileVersions, HandedOver, OverlayStatus};
use rovik_core::rt;
use rovik_protocol::work::{Change, Handed, WorkChange, WorkFile};
use rovik_protocol::{Fail, FailCode, Request};
use std::path::PathBuf;

/// Why a button that only a local chat has is refused.
pub const LOCAL_ONLY: &str = "Not available for a chat on a server yet.";

impl Remote {
    /// Refuses what only a local chat can do, for a chat on a host.
    pub fn local_only(&self, dialog_id: &str) -> Result<(), String> {
        match self.host_of(dialog_id) {
            Some(_) => Err(LOCAL_ONLY.into()),
            None => Ok(()),
        }
    }

    /// The bar's status: the imported result's, with the files the chat
    /// changed in its worktree so far, and its last sync. While its host
    /// can't be reached, the imported result's files.
    pub async fn overlay_status(&self, dialog_id: &str) -> Result<OverlayStatus, String> {
        let mut status = chats::overlay_status(dialog_id.to_string()).await?;
        status.sync = self.sync_state(dialog_id);
        let ask = Request::WorkStatus {
            dialog_id: dialog_id.to_string(),
        };
        match self.ask::<Vec<WorkChange>>(dialog_id, ask).await {
            Ok(changes) => {
                status.isolated = true;
                status.files = changes.into_iter().map(changed_file).collect();
            }
            Err(fail) => log::warn!("{dialog_id}'s changes there: {fail}"),
        }
        Ok(status)
    }

    /// A changed file as the project has it here and as the chat has it on
    /// its host.
    pub async fn overlay_file(&self, dialog_id: &str, path: &str) -> Result<FileVersions, String> {
        let ask = Request::WorkFile {
            dialog_id: dialog_id.to_string(),
            path: path.to_string(),
        };
        let file: WorkFile = self.ask(dialog_id, ask).await.map_err(|f| f.message)?;
        let project = self.root_of(dialog_id).await?.join(path);
        Ok(FileVersions {
            project: std::fs::read_to_string(project).ok(),
            chat: file.chat,
        })
    }

    /// What changes the chat's files waits for its run's end, on its host.
    pub async fn check_idle(&self, dialog_id: &str) -> Result<(), String> {
        let status: AgentStatus = self
            .ask(dialog_id, Request::AgentStatus)
            .await
            .map_err(|f| f.message)?;
        if status.runs.iter().any(|run| run.dialog_id == dialog_id) {
            return Err("this chat's run is still going: stop it first".into());
        }
        Ok(())
    }

    /// Throws the chat's changes away: its worktree on its host back at its
    /// landing base, and the result imported here.
    pub async fn discard(&self, dialog_id: &str) -> Result<(), String> {
        let ask = Request::WorkDiscard {
            dialog_id: dialog_id.to_string(),
        };
        self.ask::<()>(dialog_id, ask)
            .await
            .map_err(|f| f.message)?;
        self.drop_import(dialog_id).await
    }

    /// Hands the imported result's conflicts to the chat: markers in its
    /// files on its host, between its version and the project's here, and
    /// the message that asks it to settle them. The import goes: the chat's
    /// next result brings its resolution.
    pub async fn conflicts_to_chat(&self, dialog_id: &str) -> Result<HandedOver, String> {
        let pending = chats::pending(dialog_id).map(|p| p.conflicts);
        let files: Vec<String> = pending
            .unwrap_or_default()
            .into_iter()
            .map(|c| c.path)
            .collect();
        if files.is_empty() {
            return Ok(HandedOver {
                files,
                request: String::new(),
            });
        }
        let host = self
            .host_of(dialog_id)
            .ok_or("This chat isn't on a host.")?;
        let client = self.client(&host).map_err(|f| f.message)?;
        let snapshot = self.snapshot(dialog_id).await?;
        let snapshot = snapshot.ok_or("its project has no folder on this computer")?;
        self.carry(&host, &client, dialog_id, &snapshot).await?;
        let hand = Request::WorkConflictsToChat {
            dialog_id: dialog_id.to_string(),
            snapshot: snapshot.commit.to_string(),
            files,
        };
        let handed: Handed = client.request(hand).await.map_err(|f| f.message)?;
        if handed.files.is_empty() {
            return Ok(HandedOver {
                files: handed.files,
                request: String::new(),
            });
        }
        // Their landing base moved there: the next message syncs again.
        self.lock().last_sync.remove(dialog_id);
        self.drop_import(dialog_id).await?;
        let request = chats::resolve_request(&handed.files);
        Ok(HandedOver {
            files: handed.files,
            request,
        })
    }

    /// A remote chat's plan: its file under `.rovik/plans/` in the chat's
    /// worktree. The plan names it by the server's path.
    pub async fn read_plan(&self, dialog_id: &str, path: &str) -> Result<String, String> {
        let plans = ".rovik/plans/";
        let at = path.replace('\\', "/");
        let at = at
            .find(plans)
            .map(|start| &at[start..])
            .ok_or("Not a plan's file.")?;
        let ask = Request::WorkFile {
            dialog_id: dialog_id.to_string(),
            path: at.to_string(),
        };
        let file: WorkFile = self.ask(dialog_id, ask).await.map_err(|f| f.message)?;
        file.chat
            .ok_or_else(|| "The plan isn't there any more.".into())
    }

    /// A request of the chat's host.
    async fn ask<T: TryFrom<rovik_protocol::Response, Error = Fail>>(
        &self,
        dialog_id: &str,
        request: Request,
    ) -> Result<T, Fail> {
        let host = self
            .host_of(dialog_id)
            .ok_or_else(|| Fail::new(FailCode::BadRequest, "This chat isn't on a host."))?;
        self.client(&host)?.request(request).await
    }

    /// The chat's project folder here.
    async fn root_of(&self, dialog_id: &str) -> Result<PathBuf, String> {
        let project = self.project_of(dialog_id).await?;
        project
            .map(|(_, root)| root)
            .ok_or_else(|| "its project has no folder on this computer".into())
    }

    /// The result imported here goes.
    async fn drop_import(&self, dialog_id: &str) -> Result<(), String> {
        let id = dialog_id.to_string();
        let dropped = rt::spawn_blocking(move || chats::discard(&id));
        dropped.await.map_err(|e| e.to_string())?
    }
}

/// A change of the worktree as the bar lists it.
fn changed_file(change: WorkChange) -> ChangedFile {
    ChangedFile {
        path: change.path,
        change: match change.change {
            Change::Added => "added",
            Change::Changed => "changed",
            Change::Deleted => "deleted",
        },
        from: None,
    }
}

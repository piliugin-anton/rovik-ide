//! The isolation bar of a remote chat (6.1): its status, from the imported
//! result and from the files the chat changed in its worktree so far, with
//! its last sync; a changed file's versions; and what the bar's buttons do.
//! Landing, Undo and the conflict resolver work on the imported result
//! here, but "Is the run going?" is asked of the chat's host, and Undo the
//! merge and Unmark take a merge or markers back out there. Redo
//! sequentially stays a local chat's: server runs have no parallel groups.

use super::Remote;
use rovik_core::agent_run::AgentStatus;
use rovik_core::chat_db::workspaces;
use rovik_core::overlay::chats::{
    self, ChangedFile, FileVersions, HandedOver, OverlayStatus, UndoneMerge,
};
use rovik_core::rt;
use rovik_protocol::work::{self, Change, Handed, WorkChange, WorkFile};
use rovik_protocol::{Fail, FailCode, Request};

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
    /// changed in its worktree so far, its last sync, and whether a newer
    /// result waits for its conflicts to be settled. While its host can't
    /// be reached, the imported result's files.
    pub async fn overlay_status(&self, dialog_id: &str) -> Result<OverlayStatus, String> {
        let mut status = chats::overlay_status(dialog_id.to_string()).await?;
        status.sync = self.bar_sync(dialog_id);
        status.newer_result = self.newer_result(dialog_id);
        let ask = Request::WorkStatus {
            dialog_id: dialog_id.to_string(),
        };
        match self.ask::<Vec<WorkChange>>(dialog_id, ask).await {
            Ok(changes) => {
                status.isolated = true;
                let merged = changes.iter().filter(|c| c.merged);
                status.merged_in = merged.map(|c| c.path.clone()).collect();
                status.files = changes.into_iter().map(changed_file).collect();
            }
            Err(fail) => log::warn!("{dialog_id}'s changes there: {fail}"),
        }
        Ok(status)
    }

    /// A changed file as the project has it here and as the chat has it on
    /// its host, by the chat's path for it (a workspace chat's starts with
    /// its folder's name).
    pub async fn overlay_file(&self, dialog_id: &str, path: &str) -> Result<FileVersions, String> {
        let ask = Request::WorkFile {
            dialog_id: dialog_id.to_string(),
            path: path.to_string(),
        };
        let file: WorkFile = self.ask(dialog_id, ask).await.map_err(|f| f.message)?;
        let files = self.files_of(dialog_id).await?;
        let files = files.ok_or("its project has no folder on this computer")?;
        let project = files.folders.abs(path);
        Ok(FileVersions {
            project: project.and_then(|path| std::fs::read_to_string(path).ok()),
            chat: file.chat,
        })
    }

    /// What changes the chat's files waits for its run's end, on its host.
    pub async fn check_idle(&self, dialog_id: &str) -> Result<(), String> {
        self.check_here(dialog_id).await?;
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
    /// Refused when the chat's project is open in another window: its
    /// result lands there, and two windows landing into one project don't
    /// take turns.
    pub async fn check_here(&self, dialog_id: &str) -> Result<(), String> {
        if self.handled_elsewhere(dialog_id).await {
            return Err(
                "This chat's project is open in another Rovik window: land or discard it there."
                    .into(),
            );
        }
        Ok(())
    }

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
        let snapshot = self.snapshot(&host, dialog_id).await?;
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

    /// Takes the project's merged changes and conflict markers back out of
    /// the chat's files on its host (`files`: only those, Unmark). Its next
    /// result brings what conflicts with the project back here.
    pub async fn undo_merge(
        &self,
        dialog_id: &str,
        files: Option<Vec<String>>,
    ) -> Result<UndoneMerge, String> {
        self.check_here(dialog_id).await?;
        let ask = Request::WorkUndoMerge {
            dialog_id: dialog_id.to_string(),
            files,
        };
        let undone: work::UndoneMerge = self.ask(dialog_id, ask).await.map_err(|f| f.message)?;
        if !undone.restored.is_empty() {
            // Their landing base went back there: the next message syncs
            // again.
            self.lock().last_sync.remove(dialog_id);
        }
        Ok(UndoneMerge {
            restored: undone.restored,
            changed_since: undone.changed_since,
        })
    }

    /// A remote chat's plan: its file under `.rovik/plans/` in the chat's
    /// worktree, or a workspace chat's in one of its folders there (D28).
    /// The plan names it by the server's path, or by the chat's.
    pub async fn read_plan(&self, dialog_id: &str, path: &str) -> Result<String, String> {
        let id = dialog_id.to_string();
        let folders = self
            .core
            .db
            .read(move |c| workspaces::chat_folders(c, &id))
            .await?
            .unwrap_or_default();
        let names: Vec<String> = folders.into_iter().map(|f| f.name).collect();
        let at = plan_in_worktree(path, &names).ok_or("Not a plan's file.")?;
        let ask = Request::WorkFile {
            dialog_id: dialog_id.to_string(),
            path: at,
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

    /// The result imported here goes.
    async fn drop_import(&self, dialog_id: &str) -> Result<(), String> {
        let id = dialog_id.to_string();
        let dropped = rt::spawn_blocking(move || chats::discard(&id));
        dropped.await.map_err(|e| e.to_string())?
    }
}

/// Where a plan named by `path` (the server's path, or the chat's) is in a
/// chat's worktree: from its `.rovik/plans/`, after the name of the folder
/// it's in when that's one of a workspace chat's `folders` (D28). None for
/// a path that isn't a plan's.
fn plan_in_worktree(path: &str, folders: &[String]) -> Option<String> {
    let at = path.replace('\\', "/");
    let start = at.find(".rovik/plans/")?;
    let before = at[..start].trim_end_matches('/');
    let folder = before.rsplit('/').next().unwrap_or_default();
    let start = match !folder.is_empty() && folders.iter().any(|name| name == folder) {
        true => before.len() - folder.len(),
        false => start,
    };
    Some(at[start..].to_string())
}

/// A change of the worktree as the bar lists it.
fn changed_file(change: WorkChange) -> ChangedFile {
    ChangedFile {
        path: change.path,
        change: match change.change {
            Change::Added => "added",
            Change::Changed => "changed",
            Change::Deleted => "deleted",
            Change::Moved => "moved",
        },
        from: change.from,
    }
}

#[cfg(test)]
mod tests {
    use super::plan_in_worktree;

    #[test]
    fn a_plan_is_found_in_its_folder_of_the_worktree() {
        let folders = ["api".to_string(), "web".to_string()];
        let found = |path: &str| plan_in_worktree(path, &folders);
        // A folder chat's: from the worktree's root.
        let server = "/srv/rovik/work/c1/.rovik/plans/fix.md";
        assert_eq!(
            plan_in_worktree(server, &[]).as_deref(),
            Some(".rovik/plans/fix.md")
        );
        // A workspace chat's: in its first folder, by the server's path or
        // the chat's.
        let there = "/srv/rovik/work/c1/api/.rovik/plans/fix.md";
        assert_eq!(found(there).as_deref(), Some("api/.rovik/plans/fix.md"));
        assert_eq!(
            found("web/.rovik/plans/fix.md").as_deref(),
            Some("web/.rovik/plans/fix.md")
        );
        assert_eq!(
            found(r"C:\work\c1\api\.rovik\plans\fix.md").as_deref(),
            Some("api/.rovik/plans/fix.md")
        );
        // A folder that isn't one of the chat's isn't kept.
        assert_eq!(
            found("/srv/work/c1/docs/.rovik/plans/fix.md").as_deref(),
            Some(".rovik/plans/fix.md")
        );
        assert_eq!(found("src/main.rs"), None);
    }
}

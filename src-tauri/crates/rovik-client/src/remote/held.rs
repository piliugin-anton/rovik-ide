//! D19 and D32 for the workspace chats on hosts (workspaces-plan.md, 4.11):
//! a folder such a chat works in doesn't leave its workspace, isn't located
//! as another project, and isn't unchecked for the chat while the chat is
//! at work there or has changes in it there that haven't landed, which only
//! its host knows; nor while its host can't be asked. The core asks the
//! same of the chats here (`chat_db::api`), where the results already
//! brought here are too.

use super::Remote;
use rovik_core::chat_db::{store, workspaces};
use rovik_protocol::work::WorkChange;
use rovik_protocol::Request;

/// What holds a chat's folders on its host.
enum Held {
    /// The chat is at work there: a run, or its Autopilot task.
    AtWork,
    /// It has changes in these folders there that haven't landed.
    Changes(Vec<String>),
    /// Its host can't be asked, for this reason.
    Unasked(String),
}

impl Remote {
    /// D19: why the workspace's folder `name` can't be `verb` now
    /// ("removed", "located elsewhere"), for the workspace's chats on hosts
    /// that work in it, worded as the core words it for those here. With
    /// `changes`, a chat's changes in it there hold it too. None when no
    /// chat there holds it.
    pub async fn folder_held_there(
        &self,
        workspace_id: &str,
        name: &str,
        verb: &str,
        changes: bool,
    ) -> Result<Option<String>, String> {
        let (id, folder) = (workspace_id.to_string(), name.to_string());
        let chats = (self.core.db)
            .read(move |c| workspaces::chats_in_folder(c, &id, &folder))
            .await?;
        let mut why = Vec::new();
        for (chat, title) in chats {
            let Some(host) = self.host_of(&chat) else {
                continue;
            };
            let there = self.host_name(&host);
            let names = [name.to_string()];
            why.push(match self.held_there(&host, &chat, &names, changes).await {
                None => continue,
                Some(Held::AtWork) => {
                    format!("“{title}” is at work in it on {there} (wait for it, or stop it)")
                }
                Some(Held::Changes(_)) => format!(
                    "“{title}” has changes in it on {there} that haven't landed (land or discard them)"
                ),
                Some(Held::Unasked(e)) => format!(
                    "“{title}” is on {there}, which can't say whether it has changes in it ({e})"
                ),
            });
        }
        Ok((!why.is_empty()).then(|| format!("{name} can't be {verb} now: {}.", why.join("; "))))
    }

    /// D32: why the chat, on a host, can't work in `wanted` from now on
    /// (`"all"`, or the names checked as JSON, as `chat_set_folders` takes
    /// them): it's at work there, or has changes there that haven't landed
    /// in a folder it would leave, or its host can't be asked. None for a
    /// chat here, or one nothing holds; a folder chat's is the core's to
    /// refuse.
    pub async fn leaving_held_there(
        &self,
        dialog_id: &str,
        wanted: &str,
    ) -> Result<Option<String>, String> {
        let Some(host) = self.host_of(dialog_id) else {
            return Ok(None);
        };
        let (id, wanted) = (dialog_id.to_string(), wanted.to_string());
        let found = self
            .core
            .db
            .read(move |c| {
                let Some(dialog) = store::get_dialog(c, &id)? else {
                    return Ok(None);
                };
                let project = dialog.project_id.clone().unwrap_or_default();
                let Some(workspace) = workspaces::get(c, &project)? else {
                    return Ok(None);
                };
                let had =
                    workspaces::checked(workspace.folders.clone(), dialog.folders.as_deref())?;
                let next = workspaces::checked(workspace.folders, Some(&wanted))?;
                let left = (had.into_iter())
                    .filter(|f| !next.iter().any(|n| n.name == f.name))
                    .map(|f| f.name)
                    .collect::<Vec<_>>();
                Ok(Some((dialog.title, left)))
            })
            .await?;
        let Some((title, left)) = found else {
            return Ok(None);
        };
        let there = self.host_name(&host);
        Ok(match self.held_there(&host, dialog_id, &left, true).await {
            None => None,
            Some(Held::AtWork) => Some(format!(
                "“{title}” is at work on {there}: its folders can change once it stops."
            )),
            Some(Held::Changes(names)) => Some(format!(
                "“{title}” has changes in {} on {there} that aren't landed: land or discard them first.",
                names.join(", ")
            )),
            Some(Held::Unasked(e)) => Some(format!(
                "“{title}” is on {there}, which can't say whether it has changes in {} ({e}): try again once it's connected.",
                left.join(", ")
            )),
        })
    }

    /// What holds the chat's folders `names` on `host`: the chat at work
    /// there, or (`changes`) its changes in them there, which its host is
    /// asked for when there are folders to ask about. None when nothing
    /// does.
    async fn held_there(
        &self,
        host: &str,
        dialog_id: &str,
        names: &[String],
        changes: bool,
    ) -> Option<Held> {
        {
            let state = self.lock();
            if state.running.contains_key(dialog_id) || state.driving.contains(dialog_id) {
                return Some(Held::AtWork);
            }
        }
        if !changes || names.is_empty() {
            return None;
        }
        let unasked = |why: String| Some(Held::Unasked(why.trim_end_matches('.').to_string()));
        let client = match self.ready(host) {
            Ok(client) => client,
            Err(fail) => return unasked(fail.message),
        };
        let ask = Request::WorkStatus {
            dialog_id: dialog_id.to_string(),
        };
        let changed: Vec<WorkChange> = match client.request(ask).await {
            Ok(changed) => changed,
            Err(fail) => return unasked(fail.message),
        };
        let held: Vec<String> = (names.iter())
            .filter(|name| {
                let under = format!("{name}/");
                changed.iter().any(|c| c.path.starts_with(&under))
            })
            .cloned()
            .collect();
        (!held.is_empty()).then_some(Held::Changes(held))
    }
}

//! What a remote chat works in here (workspaces-plan.md, 4.11): its owner,
//! a project or a workspace, the name its host shows it by, and its
//! folders. A folder chat's is its project's folder, named "": its
//! project's shadow snapshots the whole repository it's in (K31), with its
//! history, as before. A workspace chat's are the folders checked for it:
//! each is snapshotted into a shadow of its own, and the workspace's
//! composite puts them together, one tree with each under its name (D30),
//! for one push and one sync. Its results come back the same way, a folder
//! at a time.

use super::Remote;
use crate::shadow::{self, workspace_excludes, Composite, Folded, Guards, Shadow};
use git2::Oid;
use rovik_core::agent_run;
use rovik_core::chat_db::{projects, store, workspaces};
use rovik_core::folders::Folders;
use rovik_core::overlay::chats;
use rovik_core::overlay::import::{ImportError, Imported};
use rovik_protocol::work::ChatResult;
use std::path::{Path, PathBuf};

/// What a remote chat works in here.
pub(super) struct ChatFiles {
    /// Its owner's ID: its project's, or its workspace's. The host keeps
    /// the chat's worktree in a project of that ID.
    pub(super) owner: String,
    /// What the host calls that project: the project's name, or the
    /// workspace's.
    pub(super) name: String,
    /// Its folders: a folder chat's one, or a workspace chat's, each by its
    /// name, in the workspace's order.
    pub(super) folders: Folders,
    /// A workspace chat's workspace's folders, by name and place, in its
    /// order: whose ignore rules each of its chats sends (4.11). Empty for
    /// a folder chat.
    workspace: Vec<(String, PathBuf)>,
}

/// A chat's files as one commit, for its host: in `shadow` (its project's,
/// or its workspace's composite), with the ignore rules the host's project
/// takes, and the chat's folder in the commit's tree ("" at its root).
pub(super) struct Taken {
    pub(super) shadow: PathBuf,
    pub(super) commit: Oid,
    pub(super) excludes: Vec<String>,
    pub(super) subpath: String,
}

/// A chat's owner as the database has it.
enum Owner {
    Project(String, Option<projects::ProjectSummary>),
    Workspace(workspaces::Workspace, Vec<workspaces::Folder>),
}

impl ChatFiles {
    /// The workspace a workspace chat's folders are of; None for a folder
    /// chat.
    pub(super) fn across(&self) -> Option<&str> {
        self.folders
            .single()
            .is_none()
            .then_some(self.owner.as_str())
    }

    /// A folder chat's folder; None for a workspace chat's.
    pub(super) fn root(&self) -> Option<&Path> {
        self.folders.single()
    }

    /// Where the chat's snapshots are pushed from and its results fetched
    /// into: its project's shadow, or its workspace's composite.
    pub(super) fn shadow(&self, data_dir: &Path) -> PathBuf {
        shadow::dir(data_dir, &self.owner)
    }

    /// The chat's files as they are now, at the chat's snapshot ref. It
    /// reads every file that changed since the last one: off the async
    /// threads.
    pub(super) fn snapshot(&self, data_dir: &Path, chat: &str) -> Result<Taken, String> {
        let Some(workspace) = self.across() else {
            let shadow = self.project_shadow(data_dir)?;
            let commit = shadow.snapshot(chat)?.commit;
            return Ok(self.taken(data_dir, &shadow, commit));
        };
        let mut parts = Vec::new();
        for (name, shadow) in self.folder_shadows(data_dir, workspace)? {
            let snapshot = shadow.snapshot(chat)?;
            parts.push((name, shadow.folder_tree(snapshot.tree)?));
        }
        let commit = self.composite(data_dir, workspace)?.compose(chat, &parts)?;
        self.composed(data_dir, commit)
    }

    /// The chat's files with its changes here folded in, for its move to a
    /// server (T09): the landing base and the chat's changes on it, and the
    /// changes as the move pushes them. Off the async threads.
    pub(super) fn fold(&self, data_dir: &Path, chat: &str) -> Result<(Folded, Taken), String> {
        let Some(workspace) = self.across() else {
            let shadow = self.project_shadow(data_dir)?;
            let folded = shadow.fold(chat, &chats::changes(chat))?;
            return Ok((folded, self.taken(data_dir, &shadow, folded.start)));
        };
        let (mut bases, mut starts) = (Vec::new(), Vec::new());
        for (name, shadow) in self.folder_shadows(data_dir, workspace)? {
            let folded = shadow.fold(chat, &chats::folder_changes(chat, &name)?)?;
            let tree = |commit: Oid| {
                let commit = shadow.repo().find_commit(commit);
                let tree = commit.map_err(|e| e.message().to_string())?.tree_id();
                shadow.folder_tree(tree)
            };
            bases.push((name.clone(), tree(folded.base)?));
            starts.push((name, tree(folded.start)?));
        }
        let composite = self.composite(data_dir, workspace)?;
        let base = composite.compose(chat, &bases)?;
        let start = composite.on(chat, base, &starts)?;
        Ok((Folded { base, start }, self.composed(data_dir, start)?))
    }

    /// The chat's `result`, fetched into its shadow, imported as its store
    /// here: a workspace chat's a folder at a time, from each folder's own
    /// shadow, all or none. Off the async threads.
    pub(super) fn import(
        &self,
        data_dir: &Path,
        result: &ChatResult,
    ) -> Result<Imported, ImportError> {
        let Some(workspace) = self.across() else {
            return self.project_shadow(data_dir)?.import(result);
        };
        let shadows = self.folder_shadows(data_dir, workspace)?;
        let composite = self.composite(data_dir, workspace)?;
        composite.import(result, workspace, &shadows)
    }

    /// What the chat's first push to a host carries that the user hears
    /// of first (6.4's guards): a folder chat's project's, as before; a
    /// workspace chat's from each of its folders, only what's in the
    /// folder, by the chat's paths (`api/…`), and no history, as theirs
    /// stays here (D30). It takes a snapshot: off the async threads.
    pub(super) fn guards(&self, data_dir: &Path, chat: &str) -> Result<Guards, String> {
        let Some(workspace) = self.across() else {
            let shadow = self.project_shadow(data_dir)?;
            return shadow.guards(&shadow.snapshot(chat)?);
        };
        let mut all = Guards::default();
        for (name, shadow) in self.folder_shadows(data_dir, workspace)? {
            let found = shadow.guards(&shadow.snapshot(chat)?)?;
            let prefix = match shadow.subpath() {
                "" => String::new(),
                subpath => format!("{subpath}/"),
            };
            let named = |path: String| {
                let rel = path.strip_prefix(prefix.as_str())?;
                Some(format!("{name}/{rel}"))
            };
            let sized = |(path, bytes): (String, u64)| named(path).map(|path| (path, bytes));
            all.large_files
                .extend(found.large_files.into_iter().filter_map(sized));
            all.lfs.extend(found.lfs.into_iter().filter_map(sized));
            all.nested
                .extend(found.nested.into_iter().filter_map(named));
            all.crlf_scripts
                .extend(found.crlf_scripts.into_iter().filter_map(named));
        }
        Ok(all)
    }

    /// Makes the chat's shadows ready to fetch into, as a snapshot leaves
    /// them: made the first time, and in line with the folders. Off the
    /// async threads.
    pub(super) fn ready(&self, data_dir: &Path) -> Result<(), String> {
        let Some(workspace) = self.across() else {
            return self.project_shadow(data_dir).map(drop);
        };
        self.folder_shadows(data_dir, workspace)?;
        self.composite(data_dir, workspace).map(drop)
    }

    /// A folder chat's project's shadow.
    fn project_shadow(&self, data_dir: &Path) -> Result<Shadow, String> {
        let root = self.root().ok_or("not a folder chat's files")?;
        Shadow::open(&self.shadow(data_dir), root)
    }

    /// Each of a workspace chat's folders, with its shadow.
    fn folder_shadows(
        &self,
        data_dir: &Path,
        workspace: &str,
    ) -> Result<Vec<(String, Shadow)>, String> {
        let open = |folder: &rovik_core::folders::Folder| {
            let dir = shadow::folder_dir(data_dir, workspace, &folder.name);
            Shadow::open(&dir, &folder.path).map(|shadow| (folder.name.clone(), shadow))
        };
        self.folders.iter().map(open).collect()
    }

    /// The workspace's composite, reading every folder shadow it has had:
    /// opened after the chat's own, which makes theirs the first time.
    fn composite(&self, data_dir: &Path, workspace: &str) -> Result<Composite, String> {
        let shadows = shadow::folder_dirs(data_dir, workspace);
        Composite::open(&self.shadow(data_dir), &shadows)
    }

    /// A folder chat's `commit` in its project's `shadow`.
    fn taken(&self, data_dir: &Path, shadow: &Shadow, commit: Oid) -> Taken {
        Taken {
            shadow: self.shadow(data_dir),
            commit,
            excludes: shadow.excludes(),
            subpath: shadow.subpath().to_string(),
        }
    }

    /// A workspace chat's `commit` in the composite: with the rules of all
    /// the workspace's folders, the same for each of its chats, as the host
    /// keeps one set for the project and each sync rewrites it.
    fn composed(&self, data_dir: &Path, commit: Oid) -> Result<Taken, String> {
        Ok(Taken {
            shadow: self.shadow(data_dir),
            commit,
            excludes: workspace_excludes(&self.workspace)?,
            subpath: String::new(),
        })
    }
}

impl Remote {
    /// What the chat works in here, in place of its project's folder: none
    /// for a chat without a project, or whose project's folder isn't on
    /// this computer. A folder chat's folder is its project's in the window
    /// when it's open, else the one it was last opened from. A workspace
    /// chat's folders are those checked for it, as its runs take them:
    /// refused, naming them, while one is missing (D20).
    pub(super) async fn files_of(&self, dialog_id: &str) -> Result<Option<ChatFiles>, String> {
        let id = dialog_id.to_string();
        let found = self
            .core
            .db
            .read(move |c| {
                let Some(owner) = store::dialog_project(c, &id)? else {
                    return Ok(None);
                };
                if let Some(workspace) = workspaces::get(c, &owner)? {
                    let stored = store::dialog_folders(c, &id)?;
                    let checked =
                        workspaces::checked(workspace.folders.clone(), stored.as_deref())?;
                    return Ok(Some(Owner::Workspace(workspace, checked)));
                }
                let summary = projects::summary(c, &owner)?;
                Ok(Some(Owner::Project(owner, summary)))
            })
            .await?;
        match found {
            None => Ok(None),
            Some(Owner::Workspace(workspace, checked)) => {
                let folders = agent_run::workspace_folders(&workspace.id, checked)?;
                Ok(Some(ChatFiles {
                    owner: workspace.id,
                    name: workspace.name,
                    folders,
                    workspace: (workspace.folders.into_iter())
                        .map(|folder| (folder.name, PathBuf::from(folder.path)))
                        .collect(),
                }))
            }
            Some(Owner::Project(project, summary)) => {
                let open = self
                    .lock()
                    .open
                    .iter()
                    .find(|(id, _)| *id == project)
                    .cloned();
                let name = summary.as_ref().map(|s| s.name.clone()).unwrap_or_default();
                let root = match open {
                    Some((_, root)) => Some(root),
                    None => summary
                        .filter(|s| !s.missing)
                        .map(|s| PathBuf::from(s.path)),
                };
                Ok(root.map(|root| ChatFiles {
                    owner: project,
                    name,
                    folders: Folders::one(root),
                    workspace: Vec::new(),
                }))
            }
        }
    }
}

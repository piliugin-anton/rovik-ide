//! Workspaces: several folders opened together in one window
//! (docs/design/workspaces-plan.md).
//!
//! A workspace is a `projects` row of kind `workspace`: a name, no path of
//! its own, and the folders of `workspace_folders`, in the order it shows
//! them. Each folder stays the project it is when opened alone — its chats,
//! its settings, its ID file — so a chat started in one folder is that
//! folder's. The workspace's own chats are the workspace chats, which work
//! in some or all of its folders (4.1).
//!
//! Every folder has a name (D8), given when it joins and never changed:
//! the names are the top-level folders a workspace chat's view shows, so
//! its paths start with one (`api/src/x.ts`), and its earlier messages keep
//! naming the right folder only while a name stays with its folder. A
//! removed folder's name stays taken for the same reason, and comes back
//! with the folder.
//!
//! The file system is read before a write transaction (a folder's canonical
//! path, whether it is one, its ID file), so checks and names are compared
//! in the transaction without holding the database while a slow share
//! answers.

use super::projects::{folder_project_id, path_key};
use super::store::{err, now_ms, DbResult};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;

/// A workspace's name: trimmed, not empty, at most 80 characters. It needn't
/// be unique.
fn workspace_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("A workspace needs a name.".into());
    }
    if name.chars().count() > 80 {
        return Err("A workspace's name is at most 80 characters.".into());
    }
    Ok(name.to_string())
}

/* ---------------------------------------------------------------------------
 * Types
 * ------------------------------------------------------------------------- */

/// One folder of a workspace: the name its chats write before one of its
/// paths, and the path it was opened as.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    pub name: String,
    pub path: String,
    /// The folder isn't there (deleted, or a drive that isn't mounted, D20).
    pub missing: bool,
}

/// A workspace as the window opens it.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub folders: Vec<Folder>,
}

/// A workspace as the project list offers it, with its chats'.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub id: String,
    pub name: String,
    pub folders: Vec<Folder>,
    pub chat_count: i64,
    pub last_opened_at: i64,
}

/// A folder as the file system has it now, read before a write transaction:
/// what the checks and the names compare.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    /// As it was opened: shown, and the key its settings use.
    pub path: String,
    /// Its path with links and `..` resolved, compared by (projects::path_key).
    pub key: String,
    /// The project its ID file names, if it has one.
    pub project: Option<String>,
    /// It is a folder now.
    pub is_folder: bool,
}

/// Reads a folder from the file system.
pub fn prepare(folder: &str) -> Prepared {
    let path = Path::new(folder);
    Prepared {
        path: folder.to_string(),
        key: path_key(path),
        project: folder_project_id(path),
        is_folder: path.is_dir(),
    }
}

/// Why a folder can't join a workspace (D5), naming the folder it's about.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum FolderProblem {
    /// It's a folder of the workspace already, spelled another way.
    #[serde(rename_all = "camelCase")]
    Duplicate { other: String },
    /// It's inside another of the workspace's folders.
    #[serde(rename_all = "camelCase")]
    Inside { other: String },
    /// It holds another of the workspace's folders.
    #[serde(rename_all = "camelCase")]
    Contains { other: String },
    /// Its ID file names another member's project: a copy sharing its chats.
    #[serde(rename_all = "camelCase")]
    SameProject { other: String },
    /// It isn't a folder.
    NotAFolder,
}

impl FolderProblem {
    /// What to tell the user about it.
    pub fn message(&self) -> String {
        match self {
            FolderProblem::Duplicate { other } => {
                format!("It's a folder of this workspace already, as {other}.")
            }
            FolderProblem::Inside { other } => format!(
                "It's inside {other}, another folder of this workspace: a workspace holds \
                 folders side by side."
            ),
            FolderProblem::Contains { other } => format!(
                "It holds {other}, another folder of this workspace: a workspace holds \
                 folders side by side."
            ),
            FolderProblem::SameProject { other } => format!(
                "It's a copy of {other}: the two share their chats, and a workspace holds \
                 that project once."
            ),
            FolderProblem::NotAFolder => "That isn't a folder.".into(),
        }
    }
}

impl std::fmt::Display for FolderProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/* ---------------------------------------------------------------------------
 * Folders' names and rules
 * ------------------------------------------------------------------------- */

/// The last part of a path as it was opened, by both separators: `D:\`
/// gives `D`, `/work/api/` gives `api`.
fn last_part(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    // A drive root: `D:\` and `D:` are the drive `D`, not a name with a colon.
    let mut chars = trimmed.chars();
    if let (Some(drive), Some(':'), None) = (chars.next(), chars.next(), chars.next()) {
        if drive.is_ascii_alphabetic() && trimmed.len() == 2 {
            return &trimmed[..1];
        }
    }
    trimmed.rsplit(['/', '\\']).next().unwrap_or("")
}

/// The last part of a path's parent (`""` at a root).
fn parent_part(path: &str) -> &str {
    match path.trim_end_matches(['/', '\\']).rsplit_once(['/', '\\']) {
        Some((parent, _)) => last_part(parent),
        None => "",
    }
}

/// A folder's name made plain for the view and a command line: separators,
/// `:`, `$`, `~`, backticks, `*`, `?`, `[`, `]`, braces and control
/// characters become `-`, a leading `.`, `@` or `-` goes (the view's top
/// keeps `@context` for the agent's own files), at most 64 characters, and
/// nothing left is `folder`. So every name passes the `cd` rule
/// (`permissions.rs`, D26).
fn plain(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_control()
            || matches!(
                c,
                '/' | '\\' | ':' | '$' | '~' | '`' | '*' | '?' | '[' | ']' | '{' | '}'
            )
        {
            out.push('-');
        } else {
            out.push(c);
        }
    }
    let out: String = out
        .trim_start_matches(['.', '@', '-'])
        .chars()
        .take(64)
        .collect();
    if out.is_empty() {
        "folder".to_string()
    } else {
        out
    }
}

/// A name the agent's walkers skip (agent_tools.rs): a folder called one
/// would be invisible to it, so it takes its parent's name instead.
fn is_skipped(name: &str) -> bool {
    crate::agent_tools::is_denylisted_name(name)
}

/// A name the workspace gave already, compared in lower case: SQLite's
/// `NOCASE` folds ASCII only, and `Ä` and `ä` are one name.
fn is_taken(name: &str, taken: &[String]) -> bool {
    taken.contains(&name.to_lowercase())
}

/// A folder's stored name (D8): what its workspace's chats write before one
/// of its paths (`api/src/x.ts`). Its last part made plain; a clash takes its
/// parent's name (`app-api`), then a number (`api-2`). `taken`: every name
/// the workspace ever gave, in lower case.
pub fn folder_name(folder: &str, taken: &[String]) -> String {
    let base = plain(last_part(folder));
    let parent = plain(parent_part(folder));
    let with_parent = (!parent.is_empty()).then(|| format!("{parent}-{base}"));
    let mut name = match &with_parent {
        Some(with_parent) if is_skipped(&base) => with_parent.clone(),
        _ => base.clone(),
    };
    if !is_taken(&name, taken) {
        return name;
    }
    if let Some(with_parent) = with_parent {
        if !is_taken(&with_parent, taken) {
            return with_parent;
        }
    }
    for n in 2.. {
        name = format!("{base}-{n}");
        if !is_taken(&name, taken) {
            return name;
        }
    }
    name
}

/// Why `folder` can't join a workspace holding `members`, if it can't (D5):
/// the same folder however it's spelled (a link, a trailing separator, case
/// where the file system ignores it), one inside another or holding one, or
/// another member's project (a copy sharing its chats).
pub fn check_folder(members: &[Prepared], folder: &Prepared) -> Option<FolderProblem> {
    if !folder.is_folder {
        return Some(FolderProblem::NotAFolder);
    }
    if let Some(member) = members.iter().find(|m| m.key == folder.key) {
        return Some(FolderProblem::Duplicate {
            other: member.path.clone(),
        });
    }
    let here = Path::new(&folder.key);
    for member in members {
        let there = Path::new(&member.key);
        if here.starts_with(there) {
            return Some(FolderProblem::Inside {
                other: member.path.clone(),
            });
        }
        if there.starts_with(here) {
            return Some(FolderProblem::Contains {
                other: member.path.clone(),
            });
        }
    }
    if folder.project.is_some() {
        if let Some(member) = members.iter().find(|m| m.project == folder.project) {
            return Some(FolderProblem::SameProject {
                other: member.path.clone(),
            });
        }
    }
    None
}

/* ---------------------------------------------------------------------------
 * Reading
 * ------------------------------------------------------------------------- */

fn folders_where(conn: &Connection, id: &str, removed: bool) -> DbResult<Vec<Folder>> {
    let mut stmt = conn
        .prepare_cached(&format!(
            "SELECT name, path FROM workspace_folders
             WHERE project_id = ?1 AND removed_at IS {} ORDER BY position",
            if removed { "NOT NULL" } else { "NULL" }
        ))
        .map_err(err)?;
    let rows = stmt
        .query_map(params![id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    Ok(rows
        .into_iter()
        .map(|(name, path)| Folder {
            missing: !Path::new(&path).is_dir(),
            name,
            path,
        })
        .collect())
}

/// The workspace's folders, in the order it shows them.
pub fn members(conn: &Connection, id: &str) -> DbResult<Vec<Folder>> {
    folders_where(conn, id, false)
}

fn row_of(conn: &Connection, id: &str) -> DbResult<Option<String>> {
    conn.query_row(
        "SELECT name FROM projects WHERE id = ?1 AND kind = 'workspace'",
        params![id],
        |r| r.get(0),
    )
    .optional()
    .map_err(err)
}

/// The workspace, if there's one with that ID.
pub fn get(conn: &Connection, id: &str) -> DbResult<Option<Workspace>> {
    let Some(name) = row_of(conn, id)? else {
        return Ok(None);
    };
    Ok(Some(Workspace {
        id: id.to_string(),
        name,
        folders: members(conn, id)?,
    }))
}

/// The folders a workspace chat works in (workspaces-plan.md, 4.1): those
/// checked for it, or every one of its workspace's for `"all"` (as is a
/// chat that names none), in the workspace's order, each marked when it's
/// missing. A name the workspace no longer holds is no folder of the chat's.
/// None for a chat that isn't a workspace's.
pub fn chat_folders(conn: &Connection, dialog_id: &str) -> DbResult<Option<Vec<Folder>>> {
    let Some(project) = crate::chat_db::store::dialog_project(conn, dialog_id)? else {
        return Ok(None);
    };
    let Some(workspace) = get(conn, &project)? else {
        return Ok(None);
    };
    let checked = match crate::chat_db::store::dialog_folders(conn, dialog_id)?.as_deref() {
        None | Some("all") => return Ok(Some(workspace.folders)),
        Some(names) => serde_json::from_str::<Vec<String>>(names)
            .map_err(|e| format!("The chat's folders can't be read ({e})."))?,
    };
    Ok(Some(
        workspace
            .folders
            .into_iter()
            .filter(|folder| checked.contains(&folder.name))
            .collect(),
    ))
}

/// The workspace's chats that work in its folder `name` (those set to All
/// folders, and those that checked it), each with its title: what D19's
/// guard asks before the folder leaves or moves.
pub fn chats_in_folder(conn: &Connection, id: &str, name: &str) -> DbResult<Vec<(String, String)>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT id, title, folders FROM dialogs
             WHERE project_id = ?1 AND parent_id IS NULL ORDER BY created_at, id",
        )
        .map_err(err)?;
    let rows = stmt
        .query_map(params![id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    let mut chats = Vec::new();
    for (chat, title, folders) in rows {
        let works_in = match folders.as_deref() {
            None | Some("all") => true,
            Some(names) => serde_json::from_str::<Vec<String>>(names)
                .map_err(|e| format!("The folders of “{title}” can't be read ({e})."))?
                .iter()
                .any(|n| n == name),
        };
        if works_in {
            chats.push((chat, title));
        }
    }
    Ok(chats)
}

/// The project last opened where the workspace's folder `name` is, by its
/// path: what Locate… compares the folder found with (D19).
pub fn folder_project(conn: &Connection, id: &str, name: &str) -> DbResult<Option<String>> {
    let key: Option<String> = conn
        .query_row(
            "SELECT key FROM workspace_folders
             WHERE project_id = ?1 AND name = ?2 AND removed_at IS NULL",
            params![id, name],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?;
    let Some(key) = key else {
        return Ok(None);
    };
    conn.query_row(
        "SELECT project_id FROM project_paths WHERE path = ?1
         ORDER BY last_opened_at DESC LIMIT 1",
        params![key],
        |r| r.get(0),
    )
    .optional()
    .map_err(err)
}

/// Every workspace, most recently opened first, each folder marked when it's
/// missing.
pub fn list(conn: &Connection) -> DbResult<Vec<Summary>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT id, name, last_opened_at,
                    (SELECT COUNT(*) FROM dialogs d
                     WHERE d.project_id = p.id AND d.parent_id IS NULL)
             FROM projects p WHERE kind = 'workspace'
             ORDER BY last_opened_at DESC, rowid DESC",
        )
        .map_err(err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    drop(stmt);
    let mut out = Vec::with_capacity(rows.len());
    for (id, name, last_opened_at, chat_count) in rows {
        out.push(Summary {
            folders: members(conn, &id)?,
            id,
            name,
            chat_count,
            last_opened_at,
        });
    }
    Ok(out)
}

/// The workspace was opened: it's the most recent one in the list.
pub fn opened(conn: &Connection, id: &str) -> DbResult<()> {
    let changed = conn
        .execute(
            "UPDATE projects SET last_opened_at = ?2 WHERE id = ?1 AND kind = 'workspace'",
            params![id, now_ms()],
        )
        .map_err(err)?;
    if changed == 0 {
        return Err("That workspace isn't there any more.".into());
    }
    Ok(())
}

/* ---------------------------------------------------------------------------
 * Writing
 * ------------------------------------------------------------------------- */

/// Every name the workspace ever gave, in lower case.
fn taken_names(conn: &Connection, id: &str) -> DbResult<Vec<String>> {
    let mut stmt = conn
        .prepare_cached("SELECT name FROM workspace_folders WHERE project_id = ?1")
        .map_err(err)?;
    let names = stmt
        .query_map(params![id], |r| r.get::<_, String>(0))
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    Ok(names.iter().map(|n| n.to_lowercase()).collect())
}

/// The name the workspace gave the folder with `key`, removed or not: a
/// folder re-added comes back by its name.
fn name_of_key(conn: &Connection, id: &str, key: &str) -> DbResult<Option<String>> {
    conn.query_row(
        "SELECT name FROM workspace_folders WHERE project_id = ?1 AND key = ?2",
        params![id, key],
        |r| r.get(0),
    )
    .optional()
    .map_err(err)
}

fn next_position(conn: &Connection, id: &str) -> DbResult<i64> {
    conn.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM workspace_folders WHERE project_id = ?1",
        params![id],
        |r| r.get(0),
    )
    .map_err(err)
}

/// Adds `folders` to the workspace's rows, checked against `members` (which
/// it adds to) and named (D8), in the caller's transaction: two windows
/// can't name two folders alike or add one twice.
fn add_in(
    tx: &Connection,
    id: &str,
    members: &mut Vec<Prepared>,
    folders: &[Prepared],
) -> DbResult<Vec<Folder>> {
    let mut taken = taken_names(tx, id)?;
    let first = next_position(tx, id)?;
    let mut added = Vec::new();
    for (position, folder) in (first..).zip(folders) {
        if let Some(problem) = check_folder(members, folder) {
            return Err(problem.message());
        }
        let name = match name_of_key(tx, id, &folder.key)? {
            Some(name) => name,
            None => folder_name(&folder.path, &taken),
        };
        tx.execute(
            "INSERT INTO workspace_folders (project_id, key, name, path, position, removed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)
             ON CONFLICT (project_id, key)
             DO UPDATE SET path = ?4, position = ?5, removed_at = NULL",
            params![id, folder.key, name, folder.path, position],
        )
        .map_err(err)?;
        taken.push(name.to_lowercase());
        members.push(folder.clone());
        added.push(Folder {
            missing: !folder.is_folder,
            name,
            path: folder.path.clone(),
        });
    }
    Ok(added)
}

/// Makes a workspace named `name` holding `folders`, in this order, each
/// named as it's named (D8). A folder that can't join (D5) refuses the
/// whole: nothing is made.
pub fn create(conn: &mut Connection, name: &str, folders: &[Prepared]) -> DbResult<Workspace> {
    let name = workspace_name(name)?;
    if folders.is_empty() {
        return Err("A workspace holds at least one folder.".into());
    }
    let tx = conn.transaction().map_err(err)?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    tx.execute(
        "INSERT INTO projects (id, name, path, created_at, last_opened_at, kind)
         VALUES (?1, ?2, '', ?3, ?3, 'workspace')",
        params![id, name, now],
    )
    .map_err(err)?;
    let mut members = Vec::new();
    let made = add_in(&tx, &id, &mut members, folders)?;
    tx.commit().map_err(err)?;
    Ok(Workspace {
        id,
        name,
        folders: made,
    })
}

/// Adds `folders` to the workspace, each named as it's named (D8). One that
/// can't join (D5) refuses the whole: none is added. `known`: the
/// workspace's folders as they are now, prepared by the caller off the
/// database thread (their paths, keys and ID files read from the file system
/// before this transaction starts).
pub fn add_folders(
    conn: &mut Connection,
    id: &str,
    known: &[Prepared],
    folders: &[Prepared],
) -> DbResult<Workspace> {
    let name = row_of(conn, id)?.ok_or("That workspace isn't there any more.")?;
    let mut all = known.to_vec();
    let tx = conn.transaction().map_err(err)?;
    add_in(&tx, id, &mut all, folders)?;
    tx.commit().map_err(err)?;
    Ok(Workspace {
        id: id.to_string(),
        name,
        folders: members(conn, id)?,
    })
}

/// The chat's folders after `name` left them (`"all"`: the chat follows the
/// workspace, so nothing changes).
fn without_folder(folders: &str, name: &str) -> Option<String> {
    let checked: Vec<String> = serde_json::from_str(folders).ok()?;
    if !checked.iter().any(|f| f == name) {
        return None;
    }
    let rest: Vec<String> = checked.into_iter().filter(|f| f != name).collect();
    Some(serde_json::to_string(&rest).unwrap_or_else(|_| "[]".into()))
}

/// Removes the folder named `name` from the workspace: its name stays taken
/// (D8), so its chats' earlier paths keep naming it, and it leaves the
/// folders of the workspace's chats that had it (D19). The last folder
/// stays: a workspace holds at least one (D5).
pub fn remove_folder(conn: &mut Connection, id: &str, name: &str) -> DbResult<()> {
    let tx = conn.transaction().map_err(err)?;
    let names: Vec<String> = {
        let mut stmt = tx
            .prepare_cached(
                "SELECT name FROM workspace_folders
                 WHERE project_id = ?1 AND removed_at IS NULL ORDER BY position",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map(params![id], |r| r.get::<_, String>(0))
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err)?;
        rows
    };
    if !names.iter().any(|n| n == name) {
        return Err(format!("{name} isn't a folder of this workspace."));
    }
    if names.len() <= 1 {
        return Err(
            "A workspace holds at least one folder: add another one before removing this.".into(),
        );
    }
    tx.execute(
        "UPDATE workspace_folders SET removed_at = ?3
         WHERE project_id = ?1 AND name = ?2 AND removed_at IS NULL",
        params![id, name, now_ms()],
    )
    .map_err(err)?;
    let chats: Vec<(String, String)> = {
        let mut stmt = tx
            .prepare_cached(
                "SELECT id, folders FROM dialogs
                 WHERE project_id = ?1 AND folders IS NOT NULL AND folders != 'all'",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map(params![id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        rows
    };
    for (chat, folders) in chats {
        if let Some(rest) = without_folder(&folders, name) {
            tx.execute(
                "UPDATE dialogs SET folders = ?2 WHERE id = ?1",
                params![chat, rest],
            )
            .map_err(err)?;
        }
    }
    tx.commit().map_err(err)
}

/// Locate…: the folder named `name` is at `to` from now on. Its name stays
/// (D8): the chats that work in it keep naming its files. `known`: the
/// workspace's folders as they are now, prepared by the caller off the
/// database thread; D5 holds for the new place too (not a folder, inside or
/// holding another, or the same project as one).
pub fn relocate_folder(
    conn: &mut Connection,
    id: &str,
    name: &str,
    known: &[Prepared],
    to: &Prepared,
) -> DbResult<Workspace> {
    let workspace = row_of(conn, id)?.ok_or("That workspace isn't there any more.")?;
    let tx = conn.transaction().map_err(err)?;
    // The folder moving leaves the others to be compared with: its own key
    // is its place, not another member's.
    let key: String = tx
        .query_row(
            "SELECT key FROM workspace_folders
             WHERE project_id = ?1 AND name = ?2 AND removed_at IS NULL",
            params![id, name],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?
        .ok_or_else(|| format!("{name} isn't a folder of this workspace."))?;
    let others: Vec<Prepared> = known.iter().filter(|m| m.key != key).cloned().collect();
    if let Some(problem) = check_folder(&others, to) {
        return Err(problem.message());
    }
    tx.execute(
        "UPDATE workspace_folders SET key = ?3, path = ?4
         WHERE project_id = ?1 AND name = ?2",
        params![id, name, to.key, to.path],
    )
    .map_err(err)?;
    tx.commit().map_err(err)?;
    Ok(Workspace {
        id: id.to_string(),
        name: workspace,
        folders: members(conn, id)?,
    })
}

#[cfg(test)]
#[path = "workspaces_tests.rs"]
mod tests;

//! Projects, so chats can belong to one, and recovering them.
//!
//! A project's identity is a random ID stored inside the project folder, in
//! `.rovik/project.json`, written the first time the folder is opened. It
//! moves with the folder: renaming or moving the project keeps its chats.
//! `.rovik/.gitignore` keeps the ID out of git: chats are local, so the ID
//! only means something on this machine.
//!
//! Every folder a project was opened from is recorded (`project_paths`), and
//! that's what recovery works from:
//!   - **Copy.** The ID file is here, but the project is still alive at
//!     another known folder with the same ID: both folders claim it. Asked
//!     once: share the chats, or a separate history for the copy.
//!   - **Moved, ID file lost.** No ID file, an unknown folder, but a git
//!     repository whose fingerprint (root commit + place inside the repo)
//!     matches a project whose folders are all gone. Suggested, never
//!     assumed: forks and templates share history too.
//!   - **Missing folder.** A project none of whose folders exist anymore:
//!     listed as missing, and can be relinked to a folder or deleted.
//! Answers are remembered per folder (`project_decisions`).

use super::store::{err, now_ms, DbResult};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

const ID_FILE: &str = ".rovik/project.json";

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
    pub path: String,
    /// The ID file is in the folder. If not (read-only folder), the project
    /// is only known by its path (and its git fingerprint).
    pub id_file: bool,
}

/// A project the user may want, as offered in a suggestion or listed.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub path: String,
    pub chat_count: i64,
    pub last_opened_at: i64,
    /// None of its folders exist (or have its ID) anymore.
    pub missing: bool,
}

/// What opening a folder found, beyond the project itself.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Suggestion {
    /// This folder has the ID of a project that's also alive elsewhere.
    #[serde(rename_all = "camelCase")]
    Copy { original: ProjectSummary },
    /// No ID file, but this repository matches projects whose folders are gone.
    #[serde(rename_all = "camelCase")]
    Moved { candidates: Vec<ProjectSummary> },
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Opened {
    pub project: ProjectInfo,
    pub suggestion: Option<Suggestion>,
}

#[derive(Serialize, Deserialize)]
struct IdFile {
    id: String,
}

/* ---------------------------------------------------------------------------
 * Paths and the ID file
 * ------------------------------------------------------------------------- */

/// The form paths are stored and compared in: symlinks and `..` resolved,
/// and case ignored where the file system ignores it (Windows, macOS).
pub fn path_key(path: &Path) -> String {
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut s = resolved.to_string_lossy().into_owned();
    if cfg!(any(windows, target_os = "macos")) {
        s = s.to_lowercase();
    }
    s
}

fn read_id(root: &Path) -> Option<String> {
    let text = fs::read_to_string(root.join(ID_FILE)).ok()?;
    let parsed: IdFile = serde_json::from_str(&text).ok()?;
    let id = parsed.id.trim();
    // Only what this app writes: anything else is treated as no ID.
    (id.len() >= 8 && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .then(|| id.to_string())
}

/// Writes the ID file (and `.rovik/.gitignore` if there's none). False when
/// the folder can't be written to.
fn write_id(root: &Path, id: &str) -> bool {
    let dir = root.join(".rovik");
    if fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        let _ = fs::write(&ignore, "# Rovik IDE: the project's local ID (chats are kept on this machine).\nproject.json\n");
    }
    let body = serde_json::to_string_pretty(&IdFile { id: id.to_string() }).unwrap_or_default();
    crate::atomic_file::write_atomic(&dir.join("project.json"), format!("{body}\n").as_bytes()).is_ok()
}

fn folder_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}

/// A known folder still belongs to the project: it exists, and its ID file
/// is this project's (or absent: a folder the ID couldn't be written to).
fn alive_at(path: &str, id: &str) -> bool {
    let p = Path::new(path);
    p.is_dir() && read_id(p).is_none_or(|found| found == id)
}

/* ---------------------------------------------------------------------------
 * Git fingerprint
 * ------------------------------------------------------------------------- */

#[derive(Debug, Clone, PartialEq)]
pub struct Fingerprint {
    /// The first commit on the first-parent line from HEAD: the same in
    /// every clone, unchanged by new work.
    pub root_commit: String,
    /// Where the project sits inside the repository ("" at its root).
    pub subpath: String,
    pub remote: Option<String>,
}

/// `git@github.com:me/app.git`, `https://user@github.com/me/app/` → `github.com/me/app`.
pub fn normalize_remote(url: &str) -> String {
    let mut s = url.trim().to_lowercase();
    for prefix in ["git+", "ssh://", "https://", "http://", "git://"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
        }
    }
    if let Some(at) = s.find('@') {
        if !s[..at].contains('/') {
            s = s[at + 1..].to_string();
        }
    }
    // scp-like host:path
    if let Some(colon) = s.find(':') {
        if !s[..colon].contains('/') && !s[colon + 1..].starts_with("//") {
            s.replace_range(colon..=colon, "/");
        }
    }
    s.trim_end_matches('/').trim_end_matches(".git").trim_end_matches('/').to_string()
}

/// The folder's git fingerprint, if it's in a repository with history.
/// Walks the history once, so it's computed off the database thread.
pub fn fingerprint(root: &Path) -> Option<Fingerprint> {
    let repo = git2::Repository::discover(root).ok()?;
    let workdir = fs::canonicalize(repo.workdir()?).ok()?;
    let here = fs::canonicalize(root).ok()?;
    let subpath = here.strip_prefix(&workdir).ok()?.to_string_lossy().replace('\\', "/");
    let head = repo.head().ok()?.peel_to_commit().ok()?;
    let mut walk = repo.revwalk().ok()?;
    walk.push(head.id()).ok()?;
    walk.simplify_first_parent().ok()?;
    let mut root_commit = head.id();
    for oid in walk.flatten() {
        root_commit = oid;
    }
    let remote = repo
        .find_remote("origin")
        .ok()
        .and_then(|r| r.url().ok().map(normalize_remote));
    Some(Fingerprint { root_commit: root_commit.to_string(), subpath, remote })
}

/* ---------------------------------------------------------------------------
 * Opening a folder
 * ------------------------------------------------------------------------- */

/// `key`: the path for lookups (path_key); `display`: as it was opened.
fn record_path(tx: &Transaction, id: &str, key: &str, display: &str, name: &str, now: i64) -> DbResult<()> {
    tx.execute(
        "UPDATE projects SET name = ?2, path = ?3, last_opened_at = ?4 WHERE id = ?1",
        params![id, name, display, now],
    )
    .map_err(err)?;
    tx.execute(
        "INSERT INTO project_paths (project_id, path, last_opened_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (project_id, path) DO UPDATE SET last_opened_at = ?3",
        params![id, key, now],
    )
    .map_err(err)?;
    Ok(())
}

fn insert_project(tx: &Transaction, id: &str, name: &str, key: &str, display: &str, now: i64) -> DbResult<()> {
    tx.execute(
        "INSERT INTO projects (id, name, path, created_at, last_opened_at) VALUES (?1, ?2, ?3, ?4, ?4)",
        params![id, name, display, now],
    )
    .map_err(err)?;
    record_path(tx, id, key, display, name, now)
}

fn known_paths(conn: &Connection, id: &str) -> DbResult<Vec<String>> {
    let mut stmt = conn
        .prepare_cached("SELECT path FROM project_paths WHERE project_id = ?1 ORDER BY last_opened_at DESC")
        .map_err(err)?;
    let rows = stmt.query_map(params![id], |r| r.get::<_, String>(0)).map_err(err)?;
    rows.collect::<Result<_, _>>().map_err(err)
}

fn decided(conn: &Connection, key: &str, id: &str) -> DbResult<Option<String>> {
    conn.query_row(
        "SELECT decision FROM project_decisions WHERE path = ?1 AND project_id = ?2",
        params![key, id],
        |r| r.get(0),
    )
    .optional()
    .map_err(err)
}

fn decide(conn: &Connection, key: &str, id: &str, decision: &str) -> DbResult<()> {
    conn.execute(
        "INSERT INTO project_decisions (path, project_id, decision, decided_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (path, project_id) DO UPDATE SET decision = ?3, decided_at = ?4",
        params![key, id, decision, now_ms()],
    )
    .map_err(err)?;
    Ok(())
}

pub fn summary(conn: &Connection, id: &str) -> DbResult<Option<ProjectSummary>> {
    let row: Option<(String, String, i64, i64)> = conn
        .query_row(
            "SELECT p.name, p.path, p.last_opened_at,
                    (SELECT COUNT(*) FROM dialogs d WHERE d.project_id = p.id)
             FROM projects p WHERE p.id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(err)?;
    let Some((name, path, last_opened_at, chat_count)) = row else { return Ok(None) };
    let missing = !known_paths(conn, id)?.iter().any(|p| alive_at(p, id));
    Ok(Some(ProjectSummary { id: id.to_string(), name, path, chat_count, last_opened_at, missing }))
}

/// The first part of opening a folder, in the database: which project it is.
/// `new_unknown`: a new project for a folder with no ID file and an unknown
/// path, which the git fingerprint may recognise (`finish_open`).
pub struct Resolved {
    pub opened: Opened,
    pub new_unknown: bool,
    pub needs_fingerprint: bool,
}

/// Which project a folder is:
///   1. the ID in its `.rovik/project.json`: that project, wherever the
///      folder is now (a move just updates the path) — unless the project is
///      still alive at another of its folders: then this looks like a copy;
///   2. no ID file, but a project was opened at this path: that one (the
///      file is written again);
///   3. otherwise a new project, with a new ID file (and, for a git
///      repository, a possible match by fingerprint: `finish_open`).
pub fn resolve(conn: &mut Connection, root: &Path) -> DbResult<Resolved> {
    let key = path_key(root);
    let name = folder_name(root);
    let display = root.to_string_lossy().into_owned();
    let now = now_ms();
    let tx = conn.transaction().map_err(err)?;

    let from_file = read_id(root);
    let mut suggestion = None;
    let mut new_unknown = false;
    let id = match from_file.clone() {
        Some(id) => {
            let exists: bool = tx
                .query_row("SELECT 1 FROM projects WHERE id = ?1", params![id], |_| Ok(()))
                .optional()
                .map_err(err)?
                .is_some();
            if !exists {
                // The ID came with the folder but this database never saw it
                // (a fresh install, or a folder from another machine).
                insert_project(&tx, &id, &name, &key, &display, now)?;
            } else {
                let paths = known_paths(&tx, &id)?;
                if !paths.contains(&key) {
                    let elsewhere = paths.iter().find(|p| alive_at(p, &id));
                    if elsewhere.is_some() && decided(&tx, &key, &id)?.is_none() {
                        if let Some(original) = summary(&tx, &id)? {
                            suggestion = Some(Suggestion::Copy { original });
                        }
                    }
                }
                record_path(&tx, &id, &key, &display, &name, now)?;
            }
            id
        }
        None => {
            let at_path: Option<String> = tx
                .query_row(
                    "SELECT project_id FROM project_paths WHERE path = ?1 ORDER BY last_opened_at DESC LIMIT 1",
                    params![key],
                    |r| r.get(0),
                )
                .optional()
                .map_err(err)?;
            match at_path {
                Some(id) => {
                    record_path(&tx, &id, &key, &display, &name, now)?;
                    id
                }
                None => {
                    let id = uuid::Uuid::new_v4().to_string();
                    insert_project(&tx, &id, &name, &key, &display, now)?;
                    new_unknown = true;
                    id
                }
            }
        }
    };
    let needs_fingerprint: bool = tx
        .query_row("SELECT git_root_commit IS NULL FROM projects WHERE id = ?1", params![id], |r| r.get(0))
        .map_err(err)?;
    tx.commit().map_err(err)?;

    let id_file = if from_file.as_deref() == Some(id.as_str()) { true } else { write_id(root, &id) };
    Ok(Resolved {
        opened: Opened { project: ProjectInfo { id, name, path: display, id_file }, suggestion },
        new_unknown,
        needs_fingerprint,
    })
}

/// The second part: stores the fingerprint, and for a new project from an
/// unknown folder, looks for missing projects with the same one.
pub fn finish_open(conn: &mut Connection, mut resolved: Resolved, fp: Option<Fingerprint>, root: &Path) -> DbResult<Opened> {
    let Some(fp) = fp else { return Ok(resolved.opened) };
    let id = resolved.opened.project.id.clone();
    conn.execute(
        "UPDATE projects SET git_root_commit = ?2, git_subpath = ?3, git_remote = ?4 WHERE id = ?1",
        params![id, fp.root_commit, fp.subpath, fp.remote],
    )
    .map_err(err)?;
    if !resolved.new_unknown || resolved.opened.suggestion.is_some() {
        return Ok(resolved.opened);
    }
    let key = path_key(root);
    let mut stmt = conn
        .prepare_cached(
            "SELECT id, git_remote FROM projects
             WHERE id != ?1 AND git_root_commit = ?2 AND IFNULL(git_subpath, '') = ?3",
        )
        .map_err(err)?;
    let found: Vec<(String, Option<String>)> = stmt
        .query_map(params![id, fp.root_commit, fp.subpath], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(err)?
        .collect::<Result<_, _>>()
        .map_err(err)?;
    drop(stmt);
    let mut candidates: Vec<(bool, ProjectSummary)> = Vec::new();
    for (other, remote) in found {
        if decided(conn, &key, &other)?.is_some() {
            continue;
        }
        if let Some(s) = summary(conn, &other)? {
            // Only projects that lost their folder: live clones are separate.
            if s.missing {
                let same_remote = remote.is_some() && remote == fp.remote;
                candidates.push((same_remote, s));
            }
        }
    }
    let name = folder_name(root);
    candidates.sort_by(|(ra, a), (rb, b)| {
        rb.cmp(ra)
            .then((b.name == name).cmp(&(a.name == name)))
            .then(b.last_opened_at.cmp(&a.last_opened_at))
    });
    if !candidates.is_empty() {
        resolved.opened.suggestion =
            Some(Suggestion::Moved { candidates: candidates.into_iter().map(|(_, s)| s).collect() });
    }
    Ok(resolved.opened)
}

/// Both parts at once, for tests (the command runs them around a worker thread).
#[cfg(test)]
pub fn open_project(conn: &mut Connection, root: &Path) -> DbResult<Opened> {
    let resolved = resolve(conn, root)?;
    let fp = if resolved.needs_fingerprint { fingerprint(root) } else { None };
    finish_open(conn, resolved, fp, root)
}

/* ---------------------------------------------------------------------------
 * Answers and management
 * ------------------------------------------------------------------------- */

/// Moves all of `from`'s chats and folders to `into`, then deletes `from`.
pub fn merge(tx: &Transaction, from: &str, into: &str) -> DbResult<()> {
    if from == into {
        return Ok(());
    }
    tx.execute("UPDATE dialogs SET project_id = ?2 WHERE project_id = ?1", params![from, into])
        .map_err(err)?;
    tx.execute(
        "INSERT OR IGNORE INTO project_paths (project_id, path, last_opened_at)
         SELECT ?2, path, last_opened_at FROM project_paths WHERE project_id = ?1",
        params![from, into],
    )
    .map_err(err)?;
    tx.execute("DELETE FROM projects WHERE id = ?1", params![from]).map_err(err)?;
    Ok(())
}

fn info(conn: &Connection, id: &str, root: &Path) -> DbResult<ProjectInfo> {
    let name: String = conn
        .query_row("SELECT name FROM projects WHERE id = ?1", params![id], |r| r.get(0))
        .map_err(err)?;
    Ok(ProjectInfo { id: id.to_string(), name, path: root.to_string_lossy().into_owned(), id_file: read_id(root).as_deref() == Some(id) })
}

/// "Looks like a copy": share the original's chats, or start a separate
/// history (a new ID in this folder).
pub fn answer_copy(conn: &mut Connection, root: &Path, original: &str, share: bool) -> DbResult<ProjectInfo> {
    let key = path_key(root);
    let tx = conn.transaction().map_err(err)?;
    decide(&tx, &key, original, if share { "share" } else { "separate" })?;
    if share {
        tx.commit().map_err(err)?;
        return info(conn, original, root);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let name = folder_name(root);
    tx.execute("DELETE FROM project_paths WHERE project_id = ?1 AND path = ?2", params![original, key])
        .map_err(err)?;
    insert_project(&tx, &id, &name, &key, &root.to_string_lossy(), now_ms())?;
    // Same repository: same fingerprint.
    tx.execute(
        "UPDATE projects SET (git_root_commit, git_subpath, git_remote) =
           (SELECT git_root_commit, git_subpath, git_remote FROM projects WHERE id = ?2) WHERE id = ?1",
        params![id, original],
    )
    .map_err(err)?;
    tx.commit().map_err(err)?;
    write_id(root, &id);
    info(conn, &id, root)
}

/// "Looks like a moved project": use `target`'s chats here. The project made
/// for the folder meanwhile (`current`) is merged into it.
pub fn answer_moved(conn: &mut Connection, root: &Path, current: &str, target: &str) -> DbResult<ProjectInfo> {
    let key = path_key(root);
    let tx = conn.transaction().map_err(err)?;
    merge(&tx, current, target)?;
    record_path(&tx, target, &key, &root.to_string_lossy(), &folder_name(root), now_ms())?;
    tx.commit().map_err(err)?;
    write_id(root, target);
    info(conn, target, root)
}

/// "Not this one": these projects aren't suggested for this folder again.
pub fn decline(conn: &mut Connection, root: &Path, ids: &[String]) -> DbResult<()> {
    let key = path_key(root);
    for id in ids {
        decide(conn, &key, id, "declined")?;
    }
    Ok(())
}

/// Every project, most recently opened first.
pub fn list(conn: &Connection) -> DbResult<Vec<ProjectSummary>> {
    let mut stmt = conn
        .prepare_cached("SELECT id FROM projects ORDER BY last_opened_at DESC")
        .map_err(err)?;
    let ids: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .map_err(err)?
        .collect::<Result<_, _>>()
        .map_err(err)?;
    drop(stmt);
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(s) = summary(conn, &id)? {
            out.push(s);
        }
    }
    Ok(out)
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Relinked {
    #[serde(rename_all = "camelCase")]
    Done { project: ProjectInfo },
    /// The folder is another project already; merging was not asked for.
    #[serde(rename_all = "camelCase")]
    IsOtherProject { other: ProjectSummary },
}

/// A missing project's chats, at `root`. If `root` is another project
/// already, its chats are merged into that one only when `merge_ok`.
pub fn relink(conn: &mut Connection, id: &str, root: &Path, merge_ok: bool) -> DbResult<Relinked> {
    let key = path_key(root);
    let other = read_id(root).filter(|found| found != id);
    let tx = conn.transaction().map_err(err)?;
    let target = match other {
        Some(other) => {
            let other_known: bool = tx
                .query_row("SELECT 1 FROM projects WHERE id = ?1", params![other], |_| Ok(()))
                .optional()
                .map_err(err)?
                .is_some();
            if other_known && !merge_ok {
                drop(tx);
                let other = summary(conn, &other)?.ok_or("unknown project")?;
                return Ok(Relinked::IsOtherProject { other });
            }
            if other_known {
                merge(&tx, id, &other)?;
                other
            } else {
                // An ID this database doesn't know: the folder becomes this project.
                id.to_string()
            }
        }
        None => id.to_string(),
    };
    record_path(&tx, &target, &key, &root.to_string_lossy(), &folder_name(root), now_ms())?;
    tx.commit().map_err(err)?;
    if read_id(root).as_deref() != Some(target.as_str()) {
        write_id(root, &target);
    }
    Ok(Relinked::Done { project: info(conn, &target, root)? })
}

/// A project and all its chats.
pub fn delete(conn: &mut Connection, id: &str) -> DbResult<()> {
    let chats: Vec<String> = {
        let mut stmt = conn
            .prepare_cached("SELECT id FROM dialogs WHERE project_id = ?1")
            .map_err(err)?;
        let rows = stmt.query_map(params![id], |r| r.get::<_, String>(0)).map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)?
    };
    for chat in chats {
        super::store::delete_dialog(conn, &chat)?;
    }
    conn.execute("DELETE FROM projects WHERE id = ?1", params![id]).map_err(err)?;
    Ok(())
}

#[cfg(test)]
#[path = "projects_tests.rs"]
mod tests;

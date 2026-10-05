//! Projects, so chats can belong to one.
//!
//! A project's identity is a random ID stored inside the project folder, in
//! `.rovik/project.json`, written the first time the folder is opened. It
//! moves with the folder: renaming or moving the project keeps its chats.
//! The database keeps where the project was last opened (`path`), which also
//! serves when the ID file is missing (deleted, or a folder that can't be
//! written to). `.rovik/.gitignore` keeps the ID out of git: chats are local,
//! so the ID only means something on this machine.

use super::store::{err, now_ms, DbResult};
use rusqlite::{params, Connection, OptionalExtension};
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
    /// is only known by its path, and renaming it loses the link.
    pub id_file: bool,
}

#[derive(Serialize, Deserialize)]
struct IdFile {
    id: String,
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
    root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.to_string_lossy().into_owned())
}

/// The project a folder is:
///   1. the ID in its `.rovik/project.json`: its chats, wherever the folder
///      is now (a rename or move just updates the stored path);
///   2. no ID file, but a project was opened at this path: that one (the
///      file is written again);
///   3. otherwise a new project, with a new ID file.
pub fn open_project(conn: &mut Connection, root: &Path) -> DbResult<ProjectInfo> {
    let path = root.to_string_lossy().into_owned();
    let name = folder_name(root);
    let now = now_ms();
    let tx = conn.transaction().map_err(err)?;

    let from_file = read_id(root);
    let known_at_path: Option<String> = tx
        .query_row(
            "SELECT id FROM projects WHERE path = ?1 ORDER BY last_opened_at DESC LIMIT 1",
            params![path],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?;
    let id = from_file
        .clone()
        .or(known_at_path)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    tx.execute(
        "INSERT INTO projects (id, name, path, created_at, last_opened_at) VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT (id) DO UPDATE SET name = ?2, path = ?3, last_opened_at = ?4",
        params![id, name, path, now],
    )
    .map_err(err)?;
    tx.commit().map_err(err)?;

    let id_file = if from_file.as_deref() == Some(id.as_str()) { true } else { write_id(root, &id) };
    Ok(ProjectInfo { id, name, path, id_file })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_db::schema;
    use crate::chat_db::store::{create_dialog, list_dialogs, NewDialog};

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        schema::configure(&conn, true).unwrap();
        schema::migrate(&mut conn).unwrap();
        conn
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("proj-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn chat(conn: &mut Connection, id: &str, project: Option<&str>) {
        create_dialog(
            conn,
            &NewDialog { id: id.into(), title: id.into(), created_at: 1, updated_at: 1, project_id: project.map(Into::into) },
        )
        .unwrap();
    }

    fn chats(conn: &Connection, project: Option<&str>) -> Vec<String> {
        list_dialogs(conn, project, None, 50).unwrap().items.into_iter().map(|d| d.id).collect()
    }

    #[test]
    fn a_new_folder_gets_an_id_file_and_keeps_it() {
        let mut conn = db();
        let dir = temp("new");
        let p = open_project(&mut conn, &dir).unwrap();
        assert!(p.id_file && dir.join(ID_FILE).is_file());
        assert!(fs::read_to_string(dir.join(".rovik/.gitignore")).unwrap().contains("project.json"));
        let again = open_project(&mut conn, &dir).unwrap();
        assert_eq!(again.id, p.id, "same folder, same project");
    }

    #[test]
    fn rename_and_move_keep_the_chats() {
        let mut conn = db();
        let dir = temp("before");
        let p = open_project(&mut conn, &dir).unwrap();
        chat(&mut conn, "c1", Some(&p.id));
        // Renamed, and moved one level deeper.
        let moved = std::env::temp_dir().join(format!("proj-{}-moved/sub/after", std::process::id()));
        let _ = fs::remove_dir_all(moved.parent().unwrap().parent().unwrap());
        fs::create_dir_all(moved.parent().unwrap()).unwrap();
        fs::rename(&dir, &moved).unwrap();
        let q = open_project(&mut conn, &moved).unwrap();
        assert_eq!(q.id, p.id);
        assert_eq!(q.path, moved.to_string_lossy());
        assert_eq!(q.name, "after");
        assert_eq!(chats(&conn, Some(&q.id)), ["c1"]);
    }

    #[test]
    fn a_deleted_id_file_is_recovered_from_the_path() {
        let mut conn = db();
        let dir = temp("deleted-id");
        let p = open_project(&mut conn, &dir).unwrap();
        fs::remove_file(dir.join(ID_FILE)).unwrap();
        let q = open_project(&mut conn, &dir).unwrap();
        assert_eq!(q.id, p.id);
        assert!(dir.join(ID_FILE).is_file(), "written again");
    }

    #[test]
    fn projects_see_only_their_chats() {
        let mut conn = db();
        let a = open_project(&mut conn, &temp("a")).unwrap();
        let b = open_project(&mut conn, &temp("b")).unwrap();
        assert_ne!(a.id, b.id);
        chat(&mut conn, "a1", Some(&a.id));
        chat(&mut conn, "b1", Some(&b.id));
        chat(&mut conn, "none1", None);
        assert_eq!(chats(&conn, Some(&a.id)), ["a1"]);
        assert_eq!(chats(&conn, Some(&b.id)), ["b1"]);
        assert_eq!(chats(&conn, None), ["none1"], "no folder open: the chats with no project");
    }

    #[test]
    fn a_damaged_id_file_is_replaced() {
        let mut conn = db();
        let dir = temp("damaged");
        fs::create_dir_all(dir.join(".rovik")).unwrap();
        fs::write(dir.join(ID_FILE), "{ not json").unwrap();
        let p = open_project(&mut conn, &dir).unwrap();
        assert!(p.id_file);
        assert_eq!(read_id(&dir).as_deref(), Some(p.id.as_str()));
    }

    #[test]
    fn search_stays_in_the_project() {
        use crate::chat_db::search::search;
        let mut conn = db();
        let a = open_project(&mut conn, &temp("search-a")).unwrap();
        let b = open_project(&mut conn, &temp("search-b")).unwrap();
        create_dialog(&mut conn, &NewDialog { id: "a1".into(), title: "Deploy pipeline fix".into(), created_at: 1, updated_at: 1, project_id: Some(a.id.clone()) }).unwrap();
        create_dialog(&mut conn, &NewDialog { id: "b1".into(), title: "Deploy pipeline fix".into(), created_at: 2, updated_at: 2, project_id: Some(b.id.clone()) }).unwrap();
        let ids = |q: &str, p: &str| search(&conn, q, Some(p), 10).unwrap().into_iter().map(|h| h.dialog.id).collect::<Vec<_>>();
        assert_eq!(ids("deploy", &a.id), ["a1"]);
        assert_eq!(ids("deploy", &b.id), ["b1"]);
        // The typo fallback (titles) is scoped too.
        assert_eq!(ids("pipelnie", &a.id), ["a1"]);
    }
}

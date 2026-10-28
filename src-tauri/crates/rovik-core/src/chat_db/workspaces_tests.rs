//! Workspaces in the database, against a real SQLite database (in memory)
//! and real folders (workspaces-plan.md, 3.1, D5, D8).

use super::*;
use crate::chat_db::schema;
use crate::chat_db::store::{create_dialog, get_dialog, NewDialog};
use std::fs;
use std::path::PathBuf;

fn db() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    schema::configure(&conn, true).unwrap();
    schema::migrate(&mut conn).unwrap();
    conn
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ws-{}-{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A folder made in `dir`, as the window opens it.
fn made(dir: &Path, name: &str) -> Prepared {
    let path = dir.join(name);
    fs::create_dir_all(&path).unwrap();
    prepare(&path.to_string_lossy())
}

/// The workspace's folders as the api layer hands them over: each read from
/// the file system before the write transaction (3.1).
fn known(conn: &Connection, id: &str) -> Vec<Prepared> {
    members(conn, id)
        .unwrap()
        .iter()
        .map(|member| prepare(&member.path))
        .collect()
}

/// `add_folders` as the database is called: its folders prepared first.
fn add(conn: &mut Connection, id: &str, folders: &[Prepared]) -> Result<Workspace, String> {
    let known = known(conn, id);
    add_folders(conn, id, &known, folders)
}

/// `relocate_folder` likewise.
fn relocate(
    conn: &mut Connection,
    id: &str,
    name: &str,
    to: &Prepared,
) -> Result<Workspace, String> {
    let known = known(conn, id);
    relocate_folder(conn, id, name, &known, to)
}

fn names(workspace: &Workspace) -> Vec<&str> {
    workspace.folders.iter().map(|f| f.name.as_str()).collect()
}

fn chat(conn: &mut Connection, id: &str, workspace: &str, folders: Option<&str>) {
    create_dialog(
        conn,
        &NewDialog {
            id: id.into(),
            title: id.into(),
            created_at: 1,
            updated_at: 1,
            project_id: Some(workspace.into()),
            folders: folders.map(Into::into),
        },
    )
    .unwrap();
}

fn stored_folders(conn: &Connection, id: &str) -> Option<String> {
    get_dialog(conn, id).unwrap().unwrap().folders
}

/* ---------------------------------------------------------------------------
 * D5: the folder rules
 * ------------------------------------------------------------------------- */

/// D5: a folder joins a workspace once, however it's spelled: a trailing
/// separator, another case where the file system ignores it, or a link to
/// it are the same folder.
#[test]
fn a_folder_joins_a_workspace_once_however_its_spelled() {
    let mut conn = db();
    let dir = temp("dup");
    let api = made(&dir, "api");
    let workspace = create(&mut conn, "Shop", std::slice::from_ref(&api)).unwrap();
    let mut added = |folder: Prepared| add(&mut conn, &workspace.id, &[folder]);

    let trailing = prepare(&format!("{}/", api.path));
    assert_eq!(
        added(trailing).unwrap_err(),
        FolderProblem::Duplicate {
            other: api.path.clone()
        }
        .message()
    );
    #[cfg(unix)]
    {
        let link = dir.join("api-link");
        std::os::unix::fs::symlink(&api.path, &link).unwrap();
        assert!(matches!(
            added(prepare(&link.to_string_lossy())).unwrap_err(),
            ref e if e.starts_with("It's a folder of this workspace already")
        ));
    }
    // Whatever its case, where the file system ignores case (macOS, Windows).
    #[cfg(any(windows, target_os = "macos"))]
    {
        let upper = prepare(&api.path.to_uppercase());
        assert!(added(upper).is_err(), "one folder, one case rule");
    }
    assert_eq!(members(&conn, &workspace.id).unwrap().len(), 1);
}

/// D5: neither a folder inside another of the workspace's, nor one holding
/// another, joins it: the tree, the watcher and search would see one folder
/// twice.
#[test]
fn a_folder_inside_another_or_holding_one_cant_join() {
    let mut conn = db();
    let dir = temp("nested");
    let app = made(&dir, "app");
    let workspace = create(&mut conn, "Shop", std::slice::from_ref(&app)).unwrap();

    let inside = made(Path::new(&app.path), "web");
    let err = add(&mut conn, &workspace.id, std::slice::from_ref(&inside)).unwrap_err();
    assert_eq!(
        err,
        FolderProblem::Inside {
            other: app.path.clone()
        }
        .message()
    );

    // And a workspace holding the inner one refuses the outer: a folder of
    // a folder is one of a folder, the other way round.
    let inner = create(&mut conn, "Inner", std::slice::from_ref(&inside)).unwrap();
    let err = add(&mut conn, &inner.id, std::slice::from_ref(&app)).unwrap_err();
    assert_eq!(
        err,
        FolderProblem::Contains {
            other: inside.path.clone()
        }
        .message()
    );
    assert_eq!(members(&conn, &inner.id).unwrap().len(), 1);
}

/// D5: two folders whose ID files name one project share their chats, and
/// an OS lock can hold a project once: a copy joins no workspace its
/// original is in.
#[test]
fn a_copy_sharing_a_projects_chats_cant_join() {
    let mut conn = db();
    let dir = temp("copy");
    let api = made(&dir, "api");
    let copy = made(&dir, "api-copy");
    // `cp -r api api-copy` copies the ID file.
    crate::chat_db::projects::write_id_file(Path::new(&copy.path), "abcdefgh-1234").unwrap();
    crate::chat_db::projects::write_id_file(Path::new(&api.path), "abcdefgh-1234").unwrap();
    let workspace = create(&mut conn, "Shop", std::slice::from_ref(&api)).unwrap();
    let err = add(&mut conn, &workspace.id, &[prepare(&copy.path)]).unwrap_err();
    assert_eq!(
        err,
        FolderProblem::SameProject {
            other: api.path.clone()
        }
        .message()
    );
}

/// A folder that isn't there can't join.
#[test]
fn what_is_not_a_folder_cant_join() {
    let mut conn = db();
    let dir = temp("gone");
    let api = made(&dir, "api");
    let workspace = create(&mut conn, "Shop", &[api]).unwrap();
    let err = add(
        &mut conn,
        &workspace.id,
        &[prepare("/no/such/folder/at/all")],
    )
    .unwrap_err();
    assert_eq!(err, FolderProblem::NotAFolder.message());
}

/* ---------------------------------------------------------------------------
 * D8: the folders' names
 * ------------------------------------------------------------------------- */

/// D8: a name the workspace gave already takes its parent's, then a number.
#[test]
fn a_name_clash_takes_the_parents_name_then_a_number() {
    assert_eq!(folder_name("/work/api", &[]), "api");
    assert_eq!(folder_name("/home/me/api", &["api".to_string()]), "me-api");
    assert_eq!(
        folder_name("/home/me/api", &["api".to_string(), "me-api".to_string()]),
        "api-2"
    );
    assert_eq!(
        folder_name(
            "/home/me/api",
            &["api".to_string(), "me-api".to_string(), "api-2".to_string()]
        ),
        "api-3"
    );
}

/// D8: names are compared in lower case, `Ä` against `ä` too (SQLite's
/// `NOCASE` folds ASCII only, so this isn't left to the index).
#[test]
fn a_name_is_taken_whatever_its_case() {
    assert_eq!(folder_name("/w/API", &["api".to_string()]), "w-API");
    assert_eq!(folder_name("/w/Ä", &["ä".to_string()]), "w-Ä");
    assert_eq!(folder_name("/w/ä", &["Ä".to_lowercase()]), "w-ä");
}

/// D8: a name the agent's walkers skip would make the folder invisible to
/// it, so it takes its parent's name.
#[test]
fn a_name_the_agent_skips_takes_its_parents() {
    assert_eq!(folder_name("/work/app/build", &[]), "app-build");
    assert_eq!(
        folder_name("/work/app/node_modules", &[]),
        "app-node_modules"
    );
    // Unless that's taken: then a number, and not the skipped name itself.
    assert_eq!(
        folder_name("/work/app/build", &["app-build".to_string()]),
        "build-2"
    );
}

/// D8: a name is a folder of the view and of a command line: separators,
/// `:`, `$`, `%`, `~`, backticks, `*`, `?`, `[`, `]` and braces are made plain,
/// and a leading `.`, `@` or `-` goes (`@context` is the agent's own).
#[test]
fn a_name_is_made_plain() {
    for (folder, name) in [
        ("/w/a$b", "a-b"),
        ("/w/100%", "100-"),
        ("/w/a~b", "a-b"),
        ("/w/a`b", "a-b"),
        ("/w/a*b", "a-b"),
        ("/w/a?b", "a-b"),
        ("/w/a[1]", "a-1-"),
        ("/w/a{b,c}", "a-b,c-"),
        ("/w/a:b", "a-b"),
        ("/w/-api", "api"),
        ("/w/.git", "git"),
        ("/w/@context", "context"),
        ("/w/...", "folder"),
    ] {
        assert_eq!(folder_name(folder, &[]), name, "{folder}");
    }
}

/// D8: a drive root is its drive, and a name is at most 64 characters.
#[test]
fn a_drive_root_is_its_drive_and_a_long_name_is_cut() {
    assert_eq!(folder_name(r"D:\", &[]), "D");
    assert_eq!(folder_name(r"C:", &[]), "C");
    let long = format!("/w/{}", "x".repeat(300));
    assert_eq!(folder_name(&long, &[]).chars().count(), 64);
}

/* ---------------------------------------------------------------------------
 * Making, naming, removing, locating
 * ------------------------------------------------------------------------- */

/// A workspace has a name and at least one folder; a folder that can't join
/// refuses the whole, and nothing is made.
#[test]
fn a_workspace_needs_a_name_and_a_folder() {
    let mut conn = db();
    let dir = temp("needs");
    let api = made(&dir, "api");
    assert!(
        create(&mut conn, "  ", std::slice::from_ref(&api)).is_err(),
        "a name"
    );
    assert!(create(&mut conn, "Shop", &[]).is_err(), "a folder");
    let twice = create(&mut conn, "Shop", &[api.clone(), api.clone()]);
    assert!(twice.is_err(), "one folder twice");
    assert!(list(&conn).unwrap().is_empty(), "nothing was made");
}

/// The last folder stays: a workspace holds at least one (D5).
#[test]
fn the_last_folder_stays() {
    let mut conn = db();
    let dir = temp("last");
    let workspace = create(&mut conn, "Shop", &[made(&dir, "api"), made(&dir, "web")]).unwrap();
    remove_folder(&mut conn, &workspace.id, "web").unwrap();
    let err = remove_folder(&mut conn, &workspace.id, "api").unwrap_err();
    assert!(err.contains("at least one folder"), "{err}");
    assert_eq!(members(&conn, &workspace.id).unwrap().len(), 1);
    // A folder it doesn't hold (removed already) isn't one to remove either.
    assert!(remove_folder(&mut conn, &workspace.id, "web").is_err());
}

/// D19: removing a folder takes it out of the folders of the workspace's
/// chats that had it; one set to **All folders** follows the workspace, and
/// a folder chat keeps its own folder (it points at a project, not at the
/// workspace).
#[test]
fn removing_a_folder_takes_it_out_of_the_chats_that_had_it() {
    let mut conn = db();
    let dir = temp("chats");
    let workspace = create(&mut conn, "Shop", &[made(&dir, "api"), made(&dir, "web")]).unwrap();
    chat(&mut conn, "both", &workspace.id, Some(r#"["api","web"]"#));
    chat(&mut conn, "all", &workspace.id, Some("all"));
    chat(&mut conn, "web-only", &workspace.id, Some(r#"["web"]"#));

    remove_folder(&mut conn, &workspace.id, "api").unwrap();

    assert_eq!(stored_folders(&conn, "both").as_deref(), Some(r#"["web"]"#));
    assert_eq!(stored_folders(&conn, "all").as_deref(), Some("all"));
    assert_eq!(
        stored_folders(&conn, "web-only").as_deref(),
        Some(r#"["web"]"#)
    );
}

/// D8: a removed folder's name is never given to another folder, and the
/// folder comes back by its own name when it's added again.
#[test]
fn a_removed_folders_name_comes_back_with_it() {
    let mut conn = db();
    let dir = temp("names");
    let api = made(&dir, "api");
    let workspace = create(&mut conn, "Shop", &[api.clone(), made(&dir, "web")]).unwrap();
    remove_folder(&mut conn, &workspace.id, "api").unwrap();

    // Another folder with that name takes its parent's.
    let other = add(&mut conn, &workspace.id, &[made(&dir.join("other"), "api")]).unwrap();
    assert_eq!(
        other
            .folders
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["web", "other-api"]
    );

    // And the folder itself gets its name back, where the workspace put it.
    let back = add(&mut conn, &workspace.id, std::slice::from_ref(&api)).unwrap();
    let names: Vec<&str> = back.folders.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["web", "other-api", "api"]);
}

/// Locate…: the folder is at another place, and keeps its name (D8), so the
/// chats that work in it keep naming its files.
#[test]
fn locating_a_folder_keeps_its_name() {
    let mut conn = db();
    let dir = temp("locate");
    let workspace = create(&mut conn, "Shop", &[made(&dir, "api"), made(&dir, "web")]).unwrap();
    let moved = made(&dir.join("moved"), "elsewhere");

    let after = relocate(&mut conn, &workspace.id, "api", &moved).unwrap();
    assert_eq!(names(&after), ["api", "web"]);
    assert_eq!(after.folders[0].path, moved.path);
    assert!(!after.folders[0].missing);

    // Two folders are one: a place another member is at is refused.
    let err = relocate(
        &mut conn,
        &workspace.id,
        "api",
        &prepare(&after.folders[1].path),
    )
    .unwrap_err();
    assert!(err.contains("already"), "{err}");
    // Located where it is already: nothing changes, and it's no clash.
    assert_eq!(
        names(&relocate(&mut conn, &workspace.id, "api", &moved).unwrap()),
        ["api", "web"]
    );
}

/// D5 holds for Locate… too: a folder is moved to a place that isn't another
/// member's, isn't inside one, doesn't hold one, and isn't that member's
/// project.
#[test]
fn locating_a_folder_to_a_place_d5_forbids_is_refused() {
    let mut conn = db();
    let dir = temp("locate-rules");
    let app = made(&dir, "app");
    let web = made(&dir, "web");
    let workspace = create(&mut conn, "Shop", &[app.clone(), web.clone()]).unwrap();

    // Inside another member, and holding one.
    let inside = made(Path::new(&web.path), "lib");
    let err = relocate(&mut conn, &workspace.id, "app", &inside).unwrap_err();
    assert_eq!(
        err,
        FolderProblem::Inside {
            other: web.path.clone()
        }
        .message()
    );
    let err = relocate(
        &mut conn,
        &workspace.id,
        "app",
        &prepare(&dir.to_string_lossy()),
    )
    .unwrap_err();
    assert_eq!(
        err,
        FolderProblem::Contains {
            other: web.path.clone()
        }
        .message()
    );
    // Not a folder.
    let err = relocate(&mut conn, &workspace.id, "app", &prepare("/no/such/folder")).unwrap_err();
    assert_eq!(err, FolderProblem::NotAFolder.message());
    // A copy of another member's project.
    let copy = made(&dir.join("copies"), "app-copy");
    crate::chat_db::projects::write_id_file(Path::new(&web.path), "abcdefgh-1234").unwrap();
    crate::chat_db::projects::write_id_file(Path::new(&copy.path), "abcdefgh-1234").unwrap();
    let known = known(&conn, &workspace.id);
    let err = relocate_folder(
        &mut conn,
        &workspace.id,
        "app",
        &known,
        &prepare(&copy.path),
    )
    .unwrap_err();
    assert_eq!(
        err,
        FolderProblem::SameProject {
            other: web.path.clone()
        }
        .message()
    );

    // None of it moved the folder.
    assert_eq!(
        members(&conn, &workspace.id)
            .unwrap()
            .iter()
            .map(|f| (f.name.as_str(), f.path.as_str()))
            .collect::<Vec<_>>(),
        [("app", app.path.as_str()), ("web", web.path.as_str())]
    );
}

#[test]
fn a_workspace_lists_its_folders_and_the_missing_ones() {
    let mut conn = db();
    let dir = temp("list");
    let api = made(&dir, "api");
    let first = create(&mut conn, "First", &[api.clone(), made(&dir, "web")]).unwrap();
    let second = create(&mut conn, "Second", &[made(&dir, "other")]).unwrap();
    // Then the two opened at times of their own (both could be this ms).
    conn.execute(
        "UPDATE projects SET last_opened_at = 10 WHERE id = ?1",
        rusqlite::params![first.id],
    )
    .unwrap();
    conn.execute(
        "UPDATE projects SET last_opened_at = 20 WHERE id = ?1",
        rusqlite::params![second.id],
    )
    .unwrap();
    fs::remove_dir_all(&api.path).unwrap();

    let listed = list(&conn).unwrap();
    let names: Vec<&str> = listed.iter().map(|w| w.name.as_str()).collect();
    assert_eq!(names, ["Second", "First"], "most recently opened first");
    assert_eq!(
        listed[1]
            .folders
            .iter()
            .map(|f| (f.name.as_str(), f.missing))
            .collect::<Vec<_>>(),
        [("api", true), ("web", false)]
    );
    assert_eq!(get(&conn, &first.id).unwrap().unwrap().folders.len(), 2);
    assert_eq!(get(&conn, &second.id).unwrap().unwrap().folders.len(), 1);
    assert!(get(&conn, "no-such-workspace").unwrap().is_none());

    // Opening one makes it the most recent.
    opened(&conn, &first.id).unwrap();
    let listed = list(&conn).unwrap();
    assert_eq!(listed[0].id, first.id);
    opened(&conn, "no-such-workspace").unwrap_err();
}

/* ---------------------------------------------------------------------------
 * 4.1: the folders a workspace chat works in
 * ------------------------------------------------------------------------- */

/// A workspace chat's run works in the folders checked for it, in the
/// workspace's order, or in every one for All folders; one that left the
/// workspace is none of them, and a folder chat has none.
#[test]
fn a_workspace_chats_folders_are_those_checked_in_the_workspaces_order() {
    let mut conn = db();
    let dir = temp("chat-folders");
    let workspace = create(
        &mut conn,
        "Shop",
        &[made(&dir, "api"), made(&dir, "web"), made(&dir, "docs")],
    )
    .unwrap();
    chat(&mut conn, "two", &workspace.id, Some(r#"["web","api"]"#));
    chat(&mut conn, "every", &workspace.id, Some("all"));
    let names = |conn: &Connection, id: &str| -> Option<Vec<String>> {
        let folders = chat_folders(conn, id).unwrap()?;
        Some(folders.into_iter().map(|f| f.name).collect())
    };
    assert_eq!(names(&conn, "two").unwrap(), ["api", "web"]);
    assert_eq!(names(&conn, "every").unwrap(), ["api", "web", "docs"]);
    let api = &chat_folders(&conn, "two").unwrap().unwrap()[0];
    assert_eq!(api.path, dir.join("api").to_string_lossy());

    remove_folder(&mut conn, &workspace.id, "web").unwrap();
    assert_eq!(names(&conn, "two").unwrap(), ["api"]);
    assert_eq!(names(&conn, "every").unwrap(), ["api", "docs"]);

    create_dialog(
        &mut conn,
        &NewDialog {
            id: "folder-chat".into(),
            title: "folder-chat".into(),
            created_at: 1,
            updated_at: 1,
            project_id: None,
            folders: None,
        },
    )
    .unwrap();
    assert_eq!(names(&conn, "folder-chat"), None);
    assert_eq!(names(&conn, "no-such-chat"), None);
}

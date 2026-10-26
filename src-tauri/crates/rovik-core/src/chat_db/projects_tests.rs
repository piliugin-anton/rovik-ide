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
        &NewDialog {
            id: id.into(),
            title: id.into(),
            created_at: 1,
            updated_at: 1,
            project_id: project.map(Into::into),
            folders: None,
        },
    )
    .unwrap();
}

fn chats(conn: &Connection, project: Option<&str>) -> Vec<String> {
    list_dialogs(conn, project, None, 50)
        .unwrap()
        .items
        .into_iter()
        .map(|d| d.id)
        .collect()
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A git repository with two commits; returns the first (the root) commit.
fn git_repo(dir: &Path) -> String {
    let repo = git2::Repository::init(dir).unwrap();
    let sig = git2::Signature::now("t", "t@t").unwrap();
    let mut parent: Option<git2::Oid> = None;
    let mut first = None;
    for n in 0..2 {
        fs::write(dir.join("f.txt"), format!("{n}")).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("f.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let parents: Vec<git2::Commit> = parent
            .iter()
            .map(|p| repo.find_commit(*p).unwrap())
            .collect();
        let refs: Vec<&git2::Commit> = parents.iter().collect();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, "c", &tree, &refs)
            .unwrap();
        first.get_or_insert(oid);
        parent = Some(oid);
    }
    repo.remote("origin", "git@github.com:me/app.git").unwrap();
    first.unwrap().to_string()
}

#[test]
fn a_new_folder_gets_an_id_file_and_keeps_it() {
    let mut conn = db();
    let dir = temp("new");
    let p = open_project(&mut conn, &dir).unwrap().project;
    assert!(p.id_file && dir.join(ID_FILE).is_file());
    assert!(fs::read_to_string(dir.join(".rovik/.gitignore"))
        .unwrap()
        .contains("project.json"));
    let again = open_project(&mut conn, &dir).unwrap();
    assert_eq!(again.project.id, p.id, "same folder, same project");
    assert_eq!(again.suggestion, None);
}

#[test]
fn rename_and_move_keep_the_chats_silently() {
    let mut conn = db();
    let dir = temp("before");
    let p = open_project(&mut conn, &dir).unwrap().project;
    chat(&mut conn, "c1", Some(&p.id));
    let moved = std::env::temp_dir().join(format!("proj-{}-moved/sub/after", std::process::id()));
    let _ = fs::remove_dir_all(moved.parent().unwrap().parent().unwrap());
    fs::create_dir_all(moved.parent().unwrap()).unwrap();
    fs::rename(&dir, &moved).unwrap();
    let q = open_project(&mut conn, &moved).unwrap();
    assert_eq!(q.project.id, p.id);
    assert_eq!(
        q.suggestion, None,
        "the old folder is gone: a move, not a copy"
    );
    assert_eq!(q.project.name, "after");
    assert_eq!(chats(&conn, Some(&p.id)), ["c1"]);
}

#[test]
fn a_deleted_id_file_is_recovered_from_the_path() {
    let mut conn = db();
    let dir = temp("deleted-id");
    let p = open_project(&mut conn, &dir).unwrap().project;
    fs::remove_file(dir.join(ID_FILE)).unwrap();
    let q = open_project(&mut conn, &dir).unwrap().project;
    assert_eq!(q.id, p.id);
    assert!(dir.join(ID_FILE).is_file(), "written again");
}

#[test]
fn projects_see_only_their_chats() {
    let mut conn = db();
    let a = open_project(&mut conn, &temp("a")).unwrap().project;
    let b = open_project(&mut conn, &temp("b")).unwrap().project;
    assert_ne!(a.id, b.id);
    chat(&mut conn, "a1", Some(&a.id));
    chat(&mut conn, "b1", Some(&b.id));
    chat(&mut conn, "none1", None);
    assert_eq!(chats(&conn, Some(&a.id)), ["a1"]);
    assert_eq!(chats(&conn, Some(&b.id)), ["b1"]);
    assert_eq!(chats(&conn, None), ["none1"]);
}

#[test]
fn a_damaged_id_file_is_replaced() {
    let mut conn = db();
    let dir = temp("damaged");
    fs::create_dir_all(dir.join(".rovik")).unwrap();
    fs::write(dir.join(ID_FILE), "{ not json").unwrap();
    let p = open_project(&mut conn, &dir).unwrap().project;
    assert!(p.id_file);
    assert_eq!(read_id(&dir).as_deref(), Some(p.id.as_str()));
}

#[test]
fn search_stays_in_the_project() {
    use crate::chat_db::search::search;
    let mut conn = db();
    let a = open_project(&mut conn, &temp("search-a")).unwrap().project;
    let b = open_project(&mut conn, &temp("search-b")).unwrap().project;
    create_dialog(
        &mut conn,
        &NewDialog {
            id: "a1".into(),
            title: "Deploy pipeline fix".into(),
            created_at: 1,
            updated_at: 1,
            project_id: Some(a.id.clone()),
            folders: None,
        },
    )
    .unwrap();
    create_dialog(
        &mut conn,
        &NewDialog {
            id: "b1".into(),
            title: "Deploy pipeline fix".into(),
            created_at: 2,
            updated_at: 2,
            project_id: Some(b.id.clone()),
            folders: None,
        },
    )
    .unwrap();
    let ids = |q: &str, p: &str| {
        search(&conn, q, Some(p), 10)
            .unwrap()
            .into_iter()
            .map(|h| h.dialog.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids("deploy", &a.id), ["a1"]);
    assert_eq!(ids("deploy", &b.id), ["b1"]);
    assert_eq!(ids("pipelnie", &a.id), ["a1"]);
}

/* --- copies --------------------------------------------------------------- */

#[test]
fn a_copy_is_detected_and_can_share_the_chats() {
    let mut conn = db();
    let original = temp("orig");
    let p = open_project(&mut conn, &original).unwrap().project;
    chat(&mut conn, "c1", Some(&p.id));
    let copy = temp("copy");
    copy_dir(&original, &copy);
    let opened = open_project(&mut conn, &copy).unwrap();
    assert_eq!(
        opened.project.id, p.id,
        "until answered, the folder's ID decides"
    );
    match &opened.suggestion {
        Some(Suggestion::Copy { original: o }) => assert_eq!(o.id, p.id),
        other => panic!("expected a copy suggestion, got {other:?}"),
    }
    answer_copy(&mut conn, &copy, &p.id, true).unwrap();
    // Answered: not asked again, and both folders show the same chats.
    assert_eq!(open_project(&mut conn, &copy).unwrap().suggestion, None);
    assert_eq!(open_project(&mut conn, &original).unwrap().suggestion, None);
}

#[test]
fn a_copy_can_start_a_separate_history() {
    let mut conn = db();
    let original = temp("orig2");
    let p = open_project(&mut conn, &original).unwrap().project;
    chat(&mut conn, "c1", Some(&p.id));
    let copy = temp("copy2");
    copy_dir(&original, &copy);
    let opened = open_project(&mut conn, &copy).unwrap();
    assert!(matches!(opened.suggestion, Some(Suggestion::Copy { .. })));
    let separate = answer_copy(&mut conn, &copy, &p.id, false).unwrap();
    assert_ne!(separate.id, p.id);
    assert_eq!(
        read_id(&copy).as_deref(),
        Some(separate.id.as_str()),
        "the copy has its own ID now"
    );
    assert!(chats(&conn, Some(&separate.id)).is_empty());
    assert_eq!(
        chats(&conn, Some(&p.id)),
        ["c1"],
        "the original keeps its chats"
    );
    let again = open_project(&mut conn, &copy).unwrap();
    assert_eq!((again.project.id, again.suggestion), (separate.id, None));
}

/* --- moved, ID file lost (git fingerprint) ---------------------------------- */

#[test]
fn a_moved_repo_without_its_id_file_is_recognised() {
    let mut conn = db();
    let old = temp("repo-old");
    git_repo(&old);
    let p = open_project(&mut conn, &old).unwrap().project;
    chat(&mut conn, "c1", Some(&p.id));
    // Moved elsewhere, and the ID file lost on the way.
    let new_place = temp("repo-new");
    fs::remove_dir_all(&new_place).unwrap();
    fs::rename(&old, &new_place).unwrap();
    fs::remove_file(new_place.join(ID_FILE)).unwrap();
    let opened = open_project(&mut conn, &new_place).unwrap();
    assert_ne!(opened.project.id, p.id, "not assumed");
    let candidates = match opened.suggestion {
        Some(Suggestion::Moved { candidates }) => candidates,
        other => panic!("expected a moved suggestion, got {other:?}"),
    };
    assert_eq!(
        candidates.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
        std::slice::from_ref(&p.id)
    );
    assert!(candidates[0].missing && candidates[0].chat_count == 1);
    // "Use those chats": the new project made meanwhile is merged in.
    chat(&mut conn, "meanwhile", Some(&opened.project.id));
    let linked = answer_moved(&mut conn, &new_place, &opened.project.id, &p.id).unwrap();
    assert_eq!(linked.id, p.id);
    let mut all = chats(&conn, Some(&p.id));
    all.sort();
    assert_eq!(all, ["c1", "meanwhile"]);
    assert_eq!(read_id(&new_place).as_deref(), Some(p.id.as_str()));
    assert_eq!(
        open_project(&mut conn, &new_place).unwrap().project.id,
        p.id
    );
}

#[test]
fn a_live_clone_is_not_suggested_and_a_decline_is_remembered() {
    let mut conn = db();
    let a = temp("clone-a");
    git_repo(&a);
    let pa = open_project(&mut conn, &a).unwrap().project;
    // A second clone (same history), with no ID file: the first is still alive.
    let b = temp("clone-b");
    copy_dir(&a, &b);
    fs::remove_file(b.join(ID_FILE)).unwrap();
    let opened = open_project(&mut conn, &b).unwrap();
    assert_ne!(opened.project.id, pa.id);
    assert_eq!(
        opened.suggestion, None,
        "a live clone is a different project"
    );

    // Now the first one's folder disappears, and a third copy turns up.
    fs::remove_dir_all(&a).unwrap();
    let c = temp("clone-c");
    copy_dir(&b, &c);
    fs::remove_file(c.join(ID_FILE)).unwrap();
    let opened = open_project(&mut conn, &c).unwrap();
    let ids: Vec<String> = match opened.suggestion {
        Some(Suggestion::Moved { candidates }) => candidates.into_iter().map(|c| c.id).collect(),
        other => panic!("expected a moved suggestion, got {other:?}"),
    };
    assert_eq!(
        ids,
        std::slice::from_ref(&pa.id),
        "only the project whose folder is gone"
    );
    decline(&mut conn, &c, &ids).unwrap();
    fs::remove_file(c.join(ID_FILE)).unwrap();
    assert_eq!(
        open_project(&mut conn, &c).unwrap().suggestion,
        None,
        "declined: not asked again"
    );
}

#[test]
fn subfolders_of_one_repo_are_not_confused() {
    let mut conn = db();
    let repo = temp("mono");
    git_repo(&repo);
    fs::create_dir_all(repo.join("web")).unwrap();
    fs::create_dir_all(repo.join("api")).unwrap();
    let web = open_project(&mut conn, &repo.join("web")).unwrap().project;
    fs::remove_dir_all(repo.join("web")).unwrap(); // web's folder is gone
    let api = open_project(&mut conn, &repo.join("api")).unwrap();
    assert_ne!(api.project.id, web.id);
    assert_eq!(
        api.suggestion, None,
        "same root commit, different place in the repo"
    );
}

#[test]
fn remotes_are_normalized() {
    for url in [
        "git@github.com:me/app.git",
        "https://github.com/me/app",
        "https://user@github.com/me/app.git/",
        "ssh://git@github.com/me/app",
    ] {
        assert_eq!(normalize_remote(url), "github.com/me/app", "{url}");
    }
}

/* --- missing folders: listing, relinking, deleting ------------------------------ */

#[test]
fn missing_projects_are_listed_and_can_be_relinked() {
    let mut conn = db();
    let dir = temp("gone");
    let p = open_project(&mut conn, &dir).unwrap().project;
    chat(&mut conn, "c1", Some(&p.id));
    let live = open_project(&mut conn, &temp("live")).unwrap().project;
    fs::remove_dir_all(&dir).unwrap();
    let listed = list(&conn).unwrap();
    let find = |id: &str| listed.iter().find(|s| s.id == id).unwrap().clone();
    assert!(find(&p.id).missing && !find(&live.id).missing);
    assert_eq!(find(&p.id).chat_count, 1);

    // Relink to a plain folder: it becomes the project.
    let target = temp("relink-target");
    match relink(&mut conn, &p.id, &target, false).unwrap() {
        Relinked::Done { project } => assert_eq!(project.id, p.id),
        other => panic!("{other:?}"),
    }
    assert_eq!(read_id(&target).as_deref(), Some(p.id.as_str()));
    assert!(!summary(&conn, &p.id).unwrap().unwrap().missing);
}

#[test]
fn relinking_onto_another_project_asks_then_merges() {
    let mut conn = db();
    let gone = temp("gone2");
    let p = open_project(&mut conn, &gone).unwrap().project;
    chat(&mut conn, "old", Some(&p.id));
    fs::remove_dir_all(&gone).unwrap();
    let other_dir = temp("other");
    let other = open_project(&mut conn, &other_dir).unwrap().project;
    chat(&mut conn, "theirs", Some(&other.id));
    assert!(matches!(
        relink(&mut conn, &p.id, &other_dir, false).unwrap(),
        Relinked::IsOtherProject { .. }
    ));
    match relink(&mut conn, &p.id, &other_dir, true).unwrap() {
        Relinked::Done { project } => {
            assert_eq!(project.id, other.id, "the folder keeps its project")
        }
        other => panic!("{other:?}"),
    }
    let mut all = chats(&conn, Some(&other.id));
    all.sort();
    assert_eq!(all, ["old", "theirs"]);
    assert!(summary(&conn, &p.id).unwrap().is_none(), "merged away");
}

#[test]
fn deleting_a_project_deletes_its_chats() {
    let mut conn = db();
    let p = open_project(&mut conn, &temp("del")).unwrap().project;
    chat(&mut conn, "c1", Some(&p.id));
    chat(&mut conn, "keep", None);
    delete(&mut conn, &p.id).unwrap();
    assert!(chats(&conn, Some(&p.id)).is_empty());
    assert_eq!(chats(&conn, None), ["keep"]);
    assert!(list(&conn).unwrap().iter().all(|s| s.id != p.id));
}

#[test]
fn a_chat_works_only_in_its_own_projects_folders() {
    let mut conn = db();
    let a_dir = temp("may-use-a");
    let b_dir = temp("may-use-b");
    let a = open_project(&mut conn, &a_dir).unwrap().project;
    open_project(&mut conn, &b_dir).unwrap();
    assert!(chat_may_use(&conn, Some(&a.id), &a_dir).unwrap());
    assert!(
        !chat_may_use(&conn, Some(&a.id), &b_dir).unwrap(),
        "another project's folder"
    );
    assert!(
        !chat_may_use(&conn, Some(&a.id), &temp("may-use-unknown")).unwrap(),
        "a folder no project has"
    );
    assert!(
        chat_may_use(&conn, None, &b_dir).unwrap(),
        "a chat with no project isn't tied to one"
    );
}

#[test]
fn a_daemon_keeps_a_project_by_the_desktops_id() {
    let mut conn = db();
    let (repo, worktree) = (temp("daemon-repo"), temp("daemon-work"));
    ensure(&mut conn, "desktop-id-1", "app", &repo).unwrap();
    ensure(&mut conn, "desktop-id-1", "app", &worktree).unwrap();
    chat(&mut conn, "c", None);
    crate::chat_db::store::set_dialog_project(&conn, "c", Some("desktop-id-1")).unwrap();

    assert_eq!(chats(&conn, Some("desktop-id-1")), ["c"]);
    assert!(chat_may_use(&conn, Some("desktop-id-1"), &worktree).unwrap());
    write_id_file(&worktree, "desktop-id-1").unwrap();
    assert_eq!(read_id(&worktree).as_deref(), Some("desktop-id-1"));
    assert!(!worktree.join(".rovik/.gitignore").exists());

    forget_path(&conn, "desktop-id-1", &worktree).unwrap();
    assert!(!chat_may_use(&conn, Some("desktop-id-1"), &worktree).unwrap());
    assert!(chat_may_use(&conn, Some("desktop-id-1"), &repo).unwrap());
}

/// A workspace is a project of the database but not one of the folders it
/// groups: it's never listed (nothing to Relink) nor summarised as one
/// (workspaces-plan.md, 3.1).
#[test]
fn a_workspace_isnt_listed_or_summarised_as_a_project() {
    let mut conn = db();
    let dir = temp("workspace");
    let api = dir.join("api");
    fs::create_dir_all(&api).unwrap();
    let workspace = crate::chat_db::workspaces::create(
        &mut conn,
        "Shop",
        &[crate::chat_db::workspaces::prepare(&api.to_string_lossy())],
    )
    .unwrap();

    assert!(
        list(&conn).unwrap().is_empty(),
        "a workspace isn't a project"
    );
    assert_eq!(summary(&conn, &workspace.id).unwrap(), None);
    // Its folder, opened alone, is one.
    open_project(&mut conn, &api).unwrap();
    let listed = list(&conn).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].path, api.to_string_lossy());
}

/// Deleting a workspace deletes its own chats and leaves its folders'
/// projects and chats (D6).
#[test]
fn deleting_a_workspace_leaves_its_folders_projects() {
    let mut conn = db();
    let dir = temp("delete-workspace");
    let api = dir.join("api");
    fs::create_dir_all(&api).unwrap();
    let project = open_project(&mut conn, &api).unwrap().project;
    chat(&mut conn, "folder-chat", Some(&project.id));
    let workspace = crate::chat_db::workspaces::create(
        &mut conn,
        "Shop",
        &[crate::chat_db::workspaces::prepare(&api.to_string_lossy())],
    )
    .unwrap();
    create_dialog(
        &mut conn,
        &NewDialog {
            id: "workspace-chat".into(),
            title: "Across".into(),
            created_at: 1,
            updated_at: 1,
            project_id: Some(workspace.id.clone()),
            folders: Some("all".into()),
        },
    )
    .unwrap();

    delete(&mut conn, &workspace.id).unwrap();

    let exists = |conn: &Connection, id: &str| {
        conn.query_row(
            "SELECT COUNT(*) FROM dialogs WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
            == 1
    };
    assert!(!exists(&conn, "workspace-chat"), "its own chat went");
    assert!(exists(&conn, "folder-chat"), "its folder's chat stayed");
    assert!(summary(&conn, &project.id).unwrap().is_some());
    assert!(crate::chat_db::workspaces::get(&conn, &workspace.id)
        .unwrap()
        .is_none());
    let folder_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM workspace_folders", [], |r| r.get(0))
        .unwrap();
    assert_eq!(folder_rows, 0, "its folder rows went with it");
}

//! A workspace chat's overlay (docs/design/workspaces-plan.md, 4.2): one
//! view of its folders, each kept in its own part of the chat's store, and
//! a folder chat's beside it. Most need a mounted view, and are skipped
//! where the virtual-files driver doesn't work.

use super::*;
use std::fs;

/// A workspace `ws-1` of `api` (`src/a.ts`, `a.txt`) and `web` (`src/b.ts`,
/// `b.txt`), and the overlays' data folder this test's alone.
struct Shop {
    dir: crate::testing::TempDir,
    _data: tokio::sync::OwnedMutexGuard<()>,
}

impl Shop {
    fn new() -> Shop {
        let dir = crate::testing::temp_dir();
        for (file, text) in [
            ("api/src/a.ts", "export const a = 'needle';\n"),
            ("api/a.txt", "1\n2\n3\n"),
            ("web/src/b.ts", "export const b = 'needle';\n"),
            ("web/b.txt", "1\n2\n3\n"),
        ] {
            let path = dir.0.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let _data = test_data(&dir.0.join("data"));
        Shop { dir, _data }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.0.join(rel)
    }

    /// The workspace's folders named `names`.
    fn folders(&self, names: &[&str]) -> Folders {
        self.folders_in("ws-1", names)
    }

    fn folders_in(&self, workspace: &str, names: &[&str]) -> Folders {
        let list = names
            .iter()
            .map(|name| Folder {
                name: name.to_string(),
                path: self.path(name),
            })
            .collect();
        Folders::named(list).unwrap().in_workspace(workspace)
    }
}

fn opened(id: &str, folders: &Folders) -> Arc<ChatOverlay> {
    match open(id, folders, Box::new(|_, _| {})) {
        Ok(o) => o,
        Err(e) => panic!("{id}: {e}"),
    }
}

fn read(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// Every file under `dir`, by its path there, with its bytes.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(at) = todo.pop() {
        for entry in fs::read_dir(&at).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                todo.push(path);
            } else {
                let rel = path
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                files.insert(rel, fs::read(&path).unwrap());
            }
        }
    }
    files
}

/// `command` run by the system's shell in `dir`: whether it succeeded, and
/// what it printed.
fn sh(dir: &Path, command: &str) -> (bool, String) {
    let out = std::process::Command::new("sh")
        .args(["-c", command])
        .current_dir(dir)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    (out.status.success(), text)
}

/// The bar's list: each file the chat changed, and how.
fn bar(id: &str) -> Vec<(String, &'static str)> {
    status(id.to_string())
        .files
        .into_iter()
        .map(|f| (f.path, f.change))
        .collect()
}

#[test]
fn a_workspace_chat_reopens_with_each_folders_changes_and_its_bar_lists_them_while_closed() {
    if !crate::vfs::mount_works() {
        return;
    }
    let shop = Shop::new();
    let id = "ws-reopen";
    let folders = shop.folders(&["api", "web"]);
    let o = opened(id, &folders);
    assert_eq!(
        read(&o.view.join("api/a.txt")).as_deref(),
        Some("1\n2\n3\n")
    );
    fs::write(o.view.join("api/a.txt"), "1\nTWO\n3\n").unwrap();
    fs::write(o.view.join("web/new.txt"), "new\n").unwrap();
    o.fs.flush();
    close(id);

    // Each folder in its own part of the chat's store.
    let store = store_dir(id).unwrap();
    assert!(store.join(FOLDERS_FILE).is_file());
    assert!(store.join("parts/api/manifest.json").is_file());
    assert!(store.join("parts/web/manifest.json").is_file());
    assert!(!store.join(PROJECT_FILE).exists() && !store.join("manifest.json").exists());

    // Closed, its bar lists both folders' changes by its paths.
    assert!(exists(id));
    assert_eq!(
        bar(id),
        [
            ("api/a.txt".to_string(), "changed"),
            ("web/new.txt".to_string(), "added")
        ]
    );
    let versions = file_versions(id.into(), "api/a.txt".into()).unwrap();
    assert_eq!(versions.project.as_deref(), Some("1\n2\n3\n"));
    assert_eq!(versions.chat.as_deref(), Some("1\nTWO\n3\n"));

    // Opened again, each folder has its changes; the projects have none.
    let o = opened(id, &folders);
    assert_eq!(
        read(&o.view.join("api/a.txt")).as_deref(),
        Some("1\nTWO\n3\n")
    );
    assert_eq!(read(&o.view.join("web/new.txt")).as_deref(), Some("new\n"));
    assert_eq!(read(&shop.path("api/a.txt")).as_deref(), Some("1\n2\n3\n"));
    assert!(!shop.path("web/new.txt").exists());
    discard(id).unwrap();
}

#[test]
fn opening_a_workspace_chats_store_as_a_folder_chats_is_refused_and_leaves_it_as_it_was() {
    if !crate::vfs::mount_works() {
        return;
    }
    let shop = Shop::new();
    let id = "ws-kind";
    let o = opened(id, &shop.folders(&["api", "web"]));
    fs::write(o.view.join("api/a.txt"), "changed\n").unwrap();
    o.fs.flush();
    close(id);
    let store = store_dir(id).unwrap();
    let before = tree(&store);
    let refused = open(id, &Folders::one(shop.path("api")), Box::new(|_, _| {}))
        .err()
        .unwrap();
    assert!(
        refused.contains("across a workspace's folders"),
        "{refused}"
    );
    assert_eq!(tree(&store), before, "the store is as it was");
    assert!(get(id).is_none());

    // And the other way: a folder chat's changes don't open across folders.
    let folder_chat = "ws-kind-folder";
    let f = opened(folder_chat, &Folders::one(shop.path("api")));
    fs::write(f.view.join("a.txt"), "changed\n").unwrap();
    f.fs.flush();
    close(folder_chat);
    let store = store_dir(folder_chat).unwrap();
    let before = tree(&store);
    let refused = open(folder_chat, &shop.folders(&["api"]), Box::new(|_, _| {}))
        .err()
        .unwrap();
    assert!(refused.contains("in one folder"), "{refused}");
    assert_eq!(tree(&store), before, "the store is as it was");
    discard(id).unwrap();
    discard(folder_chat).unwrap();
}

#[tokio::test]
async fn the_view_gains_a_folder_checked_and_drops_one_unchecked_with_its_store() {
    if !crate::vfs::mount_works() {
        return;
    }
    use crate::chat_db::store::{self as db_store, NewDialog};
    let t = crate::testing::TestCore::new();
    let _data = TEST_DATA.clone().lock_owned().await;
    init(&t.dir.0.join("overlays"));
    let base = t.dir.0.join("shop");
    let paths: Vec<String> = ["api", "web", "docs"]
        .iter()
        .map(|name| {
            fs::create_dir_all(base.join(name)).unwrap();
            fs::write(base.join(name).join("f.txt"), format!("{name}\n")).unwrap();
            base.join(name).to_string_lossy().into_owned()
        })
        .collect();
    let workspace = crate::chat_db::api::workspace_create(&t.core.db, "Shop".into(), paths)
        .await
        .unwrap();
    let (chat, project) = ("ws-match", workspace.id.clone());
    t.core
        .db
        .write(move |c| {
            db_store::create_dialog(
                c,
                &NewDialog {
                    id: chat.into(),
                    title: chat.into(),
                    created_at: 1,
                    updated_at: 1,
                    project_id: Some(project),
                    folders: Some(r#"["api","web"]"#.into()),
                },
            )
        })
        .await
        .unwrap();
    let checked = |names: &'static str| {
        let db = t.core.db.clone();
        async move {
            db.write(move |c| db_store::set_dialog_folders(c, chat, Some(names)))
                .await
                .unwrap();
            crate::agent_run::check_project(&db, chat, None)
                .await
                .unwrap()
                .unwrap()
        }
    };

    let folders = crate::agent_run::check_project(&t.core.db, chat, None)
        .await
        .unwrap()
        .unwrap();
    let o = opened(chat, &folders);
    fs::write(o.view.join("api/f.txt"), "API\n").unwrap();
    let store = store_dir(chat).unwrap();
    assert!(store.join("parts/web").is_dir());

    // A run's start matches the view to the folders as they are then.
    let again = opened(chat, &checked(r#"["api","docs"]"#).await);
    assert!(Arc::ptr_eq(&o, &again), "nothing mounted again");
    assert_eq!(read(&o.view.join("docs/f.txt")).as_deref(), Some("docs\n"));
    assert!(!o.view.join("web").exists(), "web left the view");
    assert!(!store.join("parts/web").exists(), "and its store went");
    assert_eq!(read(&o.view.join("api/f.txt")).as_deref(), Some("API\n"));
    let names: Vec<String> = o.folders().iter().map(|f| f.name.clone()).collect();
    assert_eq!(names, ["api", "docs"]);

    // A folder the chat changed can't leave it: refused, the view as it was.
    let refused = open(chat, &checked(r#"["docs"]"#).await, Box::new(|_, _| {}))
        .err()
        .unwrap();
    assert!(refused.contains("changes in api"), "{refused}");
    assert_eq!(read(&o.view.join("api/f.txt")).as_deref(), Some("API\n"));
    assert!(store.join("parts/api").is_dir());
    discard(chat).unwrap();
}

#[test]
fn the_project_changing_under_one_folder_notes_paths_with_its_name() {
    if !crate::vfs::mount_works() {
        return;
    }
    let shop = Shop::new();
    let id = "ws-note";
    let o = opened(id, &shop.folders(&["api", "web"]));
    // The chat looks at web's b.txt, and changes api's a.txt itself.
    assert!(read(&o.view.join("web/b.txt")).is_some());
    fs::write(o.view.join("api/a.txt"), "CHAT\n2\n3\n").unwrap();
    o.fs.flush();

    fs::write(shop.path("web/b.txt"), "1\n2\nTHREE\n").unwrap();
    project_changed(&shop.path("web"), &[shop.path("web/b.txt")], false);
    let note = take_note(id).unwrap();
    assert!(note.contains("web/b.txt"), "{note}");

    // A conflict in api is the chat's by its path there.
    fs::write(shop.path("api/a.txt"), "PROJECT\n2\n3\n").unwrap();
    let found = project_changed(&shop.path("api"), &[shop.path("api/a.txt")], false);
    assert_eq!(found, [(id.to_string(), "api/a.txt".to_string())]);
    let pending: Vec<String> = pending(id)
        .unwrap()
        .conflicts
        .into_iter()
        .map(|c| c.path)
        .collect();
    assert_eq!(pending, ["api/a.txt"]);
    discard(id).unwrap();
}

#[test]
fn a_folder_chat_and_a_workspace_chat_see_each_others_clashes() {
    if !crate::vfs::mount_works() {
        return;
    }
    let shop = Shop::new();
    let (ws, one) = ("ws-peer-w", "ws-peer-f");
    let w = opened(ws, &shop.folders(&["api", "web"]));
    let f = opened(one, &Folders::one(shop.path("api")));
    fs::write(w.view.join("api/a.txt"), "W\n2\n3\n").unwrap();
    fs::write(f.view.join("a.txt"), "F\n2\n3\n").unwrap();

    let mine = check_peer_conflicts(ws);
    assert_eq!(mine.len(), 1, "{mine:?}");
    assert_eq!(mine[0].path, "api/a.txt");
    assert_eq!(mine[0].peers, [one]);
    assert!(mine[0].same_lines);
    let theirs = check_peer_conflicts(one);
    assert_eq!(theirs.len(), 1, "{theirs:?}");
    assert_eq!(theirs[0].path, "a.txt", "by its own path");
    assert_eq!(theirs[0].peers, [ws]);
    discard(ws).unwrap();
    discard(one).unwrap();
}

#[test]
fn refreshing_one_folder_keeps_the_others_clashes() {
    if !crate::vfs::mount_works() {
        return;
    }
    let shop = Shop::new();
    let (ws, on_api, on_web) = ("ws-keep-w", "ws-keep-a", "ws-keep-b");
    let w = opened(ws, &shop.folders(&["api", "web"]));
    let a = opened(on_api, &Folders::one(shop.path("api")));
    let b = opened(on_web, &Folders::one(shop.path("web")));
    fs::write(w.view.join("api/a.txt"), "W\n2\n3\n").unwrap();
    fs::write(w.view.join("web/b.txt"), "W\n2\n3\n").unwrap();
    fs::write(a.view.join("a.txt"), "A\n2\n3\n").unwrap();
    fs::write(b.view.join("b.txt"), "B\n2\n3\n").unwrap();
    let paths = |clashes: Vec<PeerConflict>| -> Vec<String> {
        clashes.into_iter().map(|c| c.path).collect()
    };
    assert_eq!(paths(check_peer_conflicts(ws)), ["api/a.txt", "web/b.txt"]);

    // The chat on api compares api only, after a change of its own.
    fs::write(a.view.join("a.txt"), "A again\n2\n3\n").unwrap();
    assert_eq!(paths(check_peer_conflicts(on_api)), ["a.txt"]);
    assert_eq!(
        paths(peer_conflicts(ws)),
        ["api/a.txt", "web/b.txt"],
        "web's clash stays"
    );
    for id in [ws, on_api, on_web] {
        discard(id).unwrap();
    }
}

#[test]
fn changes_in_folder_answers_while_the_folder_is_missing() {
    if !crate::vfs::mount_works() {
        return;
    }
    let shop = Shop::new();
    let id = "ws-missing";
    let o = opened(id, &shop.folders(&["api", "web"]));
    fs::write(o.view.join("web/b.txt"), "changed\n").unwrap();
    assert_eq!(changes_in_folder(id, "web").unwrap(), ["web/b.txt"], "open");
    o.fs.flush();
    close(id);
    // web is away: a drive that isn't mounted.
    fs::rename(shop.path("web"), shop.path("web-away")).unwrap();
    assert_eq!(changes_in_folder(id, "web").unwrap(), ["web/b.txt"]);
    assert!(changes_in_folder(id, "api").unwrap().is_empty());
    assert!(changes_in_folder(id, "docs").unwrap().is_empty());
    let listed: Vec<String> = changes(id).iter().map(|c| c.path().to_string()).collect();
    assert_eq!(listed, ["web/b.txt"], "the bar still lists it");
    discard(id).unwrap();
}

#[test]
fn locating_a_folder_as_the_same_project_keeps_its_changes() {
    if !crate::vfs::mount_works() {
        return;
    }
    let shop = Shop::new();
    let id = "ws-locate";
    let o = opened(id, &shop.folders(&["api", "web"]));
    fs::write(o.view.join("web/b.txt"), "B\n").unwrap();
    o.fs.flush();
    // web moved: Locate… finds it, with the chat's view open.
    fs::rename(shop.path("web"), shop.path("web-moved")).unwrap();
    relocate(id, "web", &shop.path("web-moved")).unwrap();
    assert_eq!(read(&o.view.join("web/b.txt")).as_deref(), Some("B\n"));
    assert_eq!(
        read(&o.view.join("web/src/b.ts")).as_deref(),
        Some("export const b = 'needle';\n")
    );
    let web = |folders: &Folders| {
        folders
            .iter()
            .find(|f| f.name == "web")
            .map(|f| f.path.clone())
    };
    let moved = shop.path("web-moved").canonicalize().unwrap();
    assert_eq!(web(&o.folders()), Some(moved));

    // And with it closed: its store says where web is.
    close(id);
    fs::rename(shop.path("web-moved"), shop.path("web-2")).unwrap();
    relocate(id, "web", &shop.path("web-2")).unwrap();
    let there = shop.path("web-2").canonicalize().unwrap();
    assert_eq!(
        web(&recorded(&store_dir(id).unwrap()).unwrap()),
        Some(there)
    );
    let o = opened(id, &recorded(&store_dir(id).unwrap()).unwrap());
    assert_eq!(read(&o.view.join("web/b.txt")).as_deref(), Some("B\n"));
    land_now(id, false).unwrap().unwrap();
    assert_eq!(read(&shop.path("web-2/b.txt")).as_deref(), Some("B\n"));
}

#[test]
fn a_worker_lands_into_the_right_folders() {
    if !crate::vfs::mount_works() {
        return;
    }
    let shop = Shop::new();
    let (id, worker) = ("ws-kids", "ws-kids-w");
    let parent = opened(id, &shop.folders(&["api", "web"]));
    let w = open_child(id, worker).unwrap();
    fs::write(w.view.join("api/a.txt"), "from the worker\n").unwrap();
    fs::write(w.view.join("web/n.txt"), "new\n").unwrap();
    w.fs.flush();
    let mut landed = land_child(worker).unwrap();
    landed.sort();
    assert_eq!(landed, ["api/a.txt", "web/n.txt"]);
    assert_eq!(
        read(&parent.view.join("web/n.txt")).as_deref(),
        Some("new\n")
    );
    assert_eq!(changes_in_folder(id, "api").unwrap(), ["api/a.txt"]);
    assert_eq!(changes_in_folder(id, "web").unwrap(), ["web/n.txt"]);
    assert!(
        !shop.path("web/n.txt").exists(),
        "into the chat, not the project"
    );
    land_now(id, false).unwrap().unwrap();
    assert_eq!(
        read(&shop.path("api/a.txt")).as_deref(),
        Some("from the worker\n")
    );
    assert_eq!(read(&shop.path("web/n.txt")).as_deref(), Some("new\n"));
}

#[test]
fn cargo_target_dir_is_the_workspaces_for_each_of_its_chats() {
    if !crate::vfs::mount_works() {
        return;
    }
    let shop = Shop::new();
    let both = opened("ws-target-a", &shop.folders(&["api", "web"]));
    let web_only = opened("ws-target-b", &shop.folders(&["web"]));
    let other = opened("ws-target-c", &shop.folders_in("ws-2", &["api"]));
    let folder_chat = opened("ws-target-f", &Folders::one(shop.path("api")));
    let target = |o: &ChatOverlay| o.command_env()[0].1.clone();
    assert_eq!(target(&both), target(&web_only));
    assert_ne!(target(&both), target(&other), "another workspace's");
    assert_ne!(target(&both), target(&folder_chat), "a folder's");
    // Their commands' history is the workspace's too.
    assert_eq!(project_for(&both.view), "workspace:ws-1");
    assert_eq!(project_for(&web_only.view), "workspace:ws-1");
    for id in ["ws-target-a", "ws-target-b", "ws-target-c", "ws-target-f"] {
        discard(id).unwrap();
    }
}

#[test]
fn common_tools_work_across_the_folders_of_one_view() {
    if !crate::vfs::mount_works() {
        return;
    }
    let shop = Shop::new();
    let id = "ws-tools";
    let o = opened(id, &shop.folders(&["api", "web"]));
    let run = |command: &str| {
        let (ok, out) = sh(&o.view, command);
        assert!(ok, "{command}: {out}");
        out
    };
    assert_eq!(run("ls"), "api\nweb\n");
    assert_eq!(
        run("find . -name '*.ts' | sort"),
        "./api/src/a.ts\n./web/src/b.ts\n"
    );
    let mut found: Vec<String> = run("grep -rl needle .").lines().map(String::from).collect();
    found.sort();
    assert_eq!(found, ["./api/src/a.ts", "./web/src/b.ts"]);
    run("cp api/src/a.ts web/src/copy.ts && mkdir -p web/new/deep && echo hi > web/new/deep/f.txt");
    assert_eq!(run("cat web/src/copy.ts"), "export const a = 'needle';\n");
    assert_eq!(run("cd web && wc -l < new/deep/f.txt").trim(), "1");
    assert_eq!(
        bar(id),
        [
            ("web/new/deep/f.txt".to_string(), "added"),
            ("web/src/copy.ts".to_string(), "added")
        ]
    );
    land_now(id, false).unwrap().unwrap();
    assert_eq!(
        read(&shop.path("web/new/deep/f.txt")).as_deref(),
        Some("hi\n")
    );
    assert!(shop.path("web/src/copy.ts").is_file());
}

#[test]
fn mv_between_folders_lands_as_an_add_and_a_delete() {
    if !crate::vfs::mount_works() {
        return;
    }
    let shop = Shop::new();
    let id = "ws-mv";
    let o = opened(id, &shop.folders(&["api", "web"]));
    let (ok, out) = sh(&o.view, "mv api/src/a.ts web/src/a.ts");
    assert!(ok, "{out}");
    let mut listed = bar(id);
    listed.sort();
    assert_eq!(
        listed,
        [
            ("api/src/a.ts".to_string(), "deleted"),
            ("web/src/a.ts".to_string(), "added")
        ]
    );
    land_now(id, false).unwrap().unwrap();
    assert!(!shop.path("api/src/a.ts").exists());
    assert_eq!(
        read(&shop.path("web/src/a.ts")).as_deref(),
        Some("export const a = 'needle';\n")
    );
}

#[test]
fn the_views_root_is_read_only_to_commands() {
    if !crate::vfs::mount_works() {
        return;
    }
    let shop = Shop::new();
    let id = "ws-root";
    let o = opened(id, &shop.folders(&["api", "web"]));
    for command in [
        "touch new.txt",
        "mkdir new",
        "mv api renamed",
        "ln -s api link",
        "rmdir web",
        "chmod 777 .",
    ] {
        let (ok, out) = sh(&o.view, command);
        assert!(!ok, "{command} worked: {out}");
    }
    assert_eq!(sh(&o.view, "ls").1, "api\nweb\n");
    assert!(changes(id).is_empty(), "{:?}", changes(id));
    discard(id).unwrap();
}

#[test]
fn a_command_naming_a_folders_own_path_is_refused_where_it_wouldnt_see_the_view_there() {
    if !crate::vfs::mount_works() {
        return;
    }
    let shop = Shop::new();
    let id = "ws-real-path";
    let o = opened(id, &shop.folders(&["api", "web"]));
    let web = o
        .folders()
        .iter()
        .find(|f| f.name == "web")
        .map(|f| f.path.clone())
        .unwrap();
    let command = format!("cat {}/b.txt", web.display());
    // Rovik's own command: fine where its sandbox shows the view there.
    let redirected = o.confine().is_some_and(|c| c.redirects_path(&web));
    assert_eq!(o.command_problem(&command, true).is_none(), redirected);
    // A harness's, which runs outside the sandbox: refused either way.
    let refused = o.command_problem(&command, false).unwrap();
    assert!(
        refused.contains("its own view of its folders")
            && refused.contains(&*web.to_string_lossy()),
        "{refused}"
    );
    assert!(o.command_problem("cat web/b.txt", false).is_none());
    discard(id).unwrap();
}

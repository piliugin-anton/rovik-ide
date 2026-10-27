//! A workspace chat's view (workspaces-plan.md, 4.2), without FUSE: two
//! folders, `api` and `web`, each its own project and overlay.

use super::*;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

/// The projects and the overlays' stores, in a folder of their own.
struct Workspace {
    dir: crate::testing::TempDir,
}

impl Workspace {
    fn new() -> Workspace {
        let dir = crate::testing::temp_dir();
        for (path, text) in [
            ("api/a.txt", "api a\n"),
            ("api/src/main.rs", "fn main() {}\n"),
            ("web/a.txt", "web a\n"),
            ("web/old.txt", "old\n"),
        ] {
            let file = dir.0.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, text).unwrap();
        }
        Workspace { dir }
    }

    fn project(&self, name: &str) -> PathBuf {
        self.dir.0.join(name)
    }

    fn folders(&self, names: &[&str]) -> Folders {
        self.at(&names
            .iter()
            .map(|name| (*name, self.project(name)))
            .collect::<Vec<_>>())
    }

    fn at(&self, list: &[(&str, PathBuf)]) -> Folders {
        Folders::named(
            list.iter()
                .map(|(name, path)| Folder {
                    name: name.to_string(),
                    path: path.clone(),
                })
                .collect(),
        )
        .unwrap()
    }

    /// The view matched to `folders`, each part's store under the
    /// workspace's folder by its name and place, so a folder reopened
    /// elsewhere has a store of its own.
    fn matched(&self, view: &CompositeFs, folders: &Folders) -> Vec<(String, Arc<OverlayFs>)> {
        let stores = self.dir.0.join("stores");
        let budget = view.budget();
        view.matched(folders, |folder| {
            let store = stores.join(format!(
                "{}-{}",
                folder.name,
                folder.path.file_name().unwrap().to_string_lossy()
            ));
            OverlayFs::open_in(&folder.path, &store, budget.clone()).map(Arc::new)
        })
        .unwrap()
    }

    /// A view of `names`.
    fn view(&self, names: &[&str]) -> CompositeFs {
        let view = CompositeFs::new(Budget::new(1 << 30));
        self.matched(&view, &self.folders(names));
        view
    }
}

fn names_at_root(view: &CompositeFs) -> Vec<String> {
    view.readdir_plus(ROOT)
        .unwrap()
        .into_iter()
        .map(|(entry, _)| entry.name)
        .collect()
}

fn read(view: &CompositeFs, path: &str) -> String {
    let ino = view.resolve(path).unwrap();
    String::from_utf8(view.read(ino, 0, 1 << 20).unwrap()).unwrap()
}

/// What the view told the kernel.
fn recorded(view: &CompositeFs) -> Arc<Mutex<Vec<Change>>> {
    let told = Arc::new(Mutex::new(Vec::new()));
    let into = told.clone();
    view.set_listener(Some(Box::new(move |changes: &[Change]| {
        into.lock().unwrap().extend_from_slice(changes)
    })));
    told
}

#[test]
fn the_root_lists_each_folder_by_its_name() {
    let w = Workspace::new();
    let view = w.view(&["api", "web"]);
    assert_eq!(names_at_root(&view), ["api", "web"]);
    let root = view.getattr(ROOT).unwrap();
    assert_eq!((root.kind, root.perm), (Kind::Dir, 0o555));
    let api = view.lookup(ROOT, "api").unwrap();
    assert_eq!(api.kind, Kind::Dir);
    assert_eq!(read(&view, "api/a.txt"), "api a\n");
    assert_eq!(read(&view, "web/a.txt"), "web a\n");
    assert_eq!(view.lookup(ROOT, "docs"), Err(VfsError::NotFound));
    if cfg!(target_os = "macos") {
        assert_eq!(view.lookup(ROOT, "API").unwrap().ino, api.ino);
    }
}

#[test]
fn inode_numbers_of_two_folders_never_collide() {
    let w = Workspace::new();
    let view = w.view(&["api", "web"]);
    let (api_root, web_root) = (
        view.lookup(ROOT, "api").unwrap().ino,
        view.lookup(ROOT, "web").unwrap().ino,
    );
    let (api_a, web_a) = (
        view.resolve("api/a.txt").unwrap(),
        view.resolve("web/a.txt").unwrap(),
    );
    let all = [ROOT, api_root, web_root, api_a, web_a];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(a, b, "{all:?}");
        }
    }
    assert!(all.iter().all(|ino| *ino < 1 << 53), "{all:?}");
    // Each names its own file.
    assert_eq!(view.path_of(api_a).unwrap(), "api/a.txt");
    assert_eq!(view.path_of(web_a).unwrap(), "web/a.txt");
    assert_eq!(view.getattr(web_a).unwrap().ino, web_a);
}

#[test]
fn a_folders_root_has_the_views_root_as_its_parent() {
    let w = Workspace::new();
    let view = w.view(&["api", "web"]);
    let api = view.lookup(ROOT, "api").unwrap().ino;
    assert_eq!(view.parent_of(api).unwrap(), ROOT);
    assert_eq!(view.path_of(api).unwrap(), "api");
    let src = view.resolve("api/src").unwrap();
    assert_eq!(view.parent_of(src).unwrap(), api);
    assert_eq!(
        view.resolve_parent("api/src/main.rs").unwrap(),
        (src, "main.rs")
    );
    assert_eq!(view.resolve_parent("web").unwrap(), (ROOT, "web"));
    assert_eq!(view.resolve("").unwrap(), ROOT);
}

#[test]
fn nothing_can_be_created_linked_renamed_or_removed_at_the_root() {
    let w = Workspace::new();
    let view = w.view(&["api", "web"]);
    let api = view.lookup(ROOT, "api").unwrap().ino;
    let file = view.resolve("api/a.txt").unwrap();
    let refused = Err(VfsError::NotPermitted);
    assert_eq!(
        view.create(ROOT, "x.txt", Kind::File, 0o644, 0, 0, None)
            .map(|_| ()),
        refused
    );
    assert_eq!(
        view.create(ROOT, "docs", Kind::Dir, 0o755, 0, 0, None)
            .map(|_| ()),
        refused
    );
    assert_eq!(view.link(file, ROOT, "a.txt").map(|_| ()), refused);
    assert_eq!(
        view.rename(ROOT, "api", ROOT, "backend", RenameMode::Replace),
        refused
    );
    assert_eq!(
        view.rename(api, "a.txt", ROOT, "a.txt", RenameMode::Replace),
        refused
    );
    assert_eq!(view.unlink(ROOT, "api"), refused);
    assert_eq!(view.rmdir(ROOT, "web"), refused);
    assert_eq!(names_at_root(&view), ["api", "web"]);
}

#[test]
fn the_roots_attributes_and_extended_attributes_cant_be_set() {
    let w = Workspace::new();
    let view = w.view(&["api"]);
    let set = SetAttr {
        perm: Some(0o777),
        ..Default::default()
    };
    assert_eq!(
        view.setattr(ROOT, &set).map(|_| ()),
        Err(VfsError::NotPermitted)
    );
    assert_eq!(
        view.setxattr(ROOT, "user.x", b"1", false, false),
        Err(VfsError::NotPermitted)
    );
    assert_eq!(
        view.removexattr(ROOT, "user.x"),
        Err(VfsError::NotPermitted)
    );
    assert_eq!(view.getxattr(ROOT, "user.x"), Err(VfsError::NoAttr));
    assert_eq!(view.listxattr(ROOT).unwrap(), Vec::<String>::new());
    assert_eq!(view.getattr(ROOT).unwrap().perm, 0o555);
    // Opening and releasing it do nothing.
    assert_eq!(view.open(ROOT, false).unwrap().ino, ROOT);
    view.release(ROOT);
}

#[test]
fn a_rename_between_folders_is_a_cross_device_error() {
    let w = Workspace::new();
    let view = w.view(&["api", "web"]);
    let (api, web) = (
        view.lookup(ROOT, "api").unwrap().ino,
        view.lookup(ROOT, "web").unwrap().ino,
    );
    assert_eq!(
        view.rename(api, "a.txt", web, "from-api.txt", RenameMode::Replace),
        Err(VfsError::CrossDevice)
    );
    let file = view.resolve("api/a.txt").unwrap();
    assert_eq!(
        view.link(file, web, "linked.txt").map(|_| ()),
        Err(VfsError::CrossDevice)
    );
    // Within a folder, as ever.
    view.rename(api, "a.txt", api, "b.txt", RenameMode::Replace)
        .unwrap();
    assert_eq!(read(&view, "api/b.txt"), "api a\n");
}

#[test]
fn a_folders_kernel_notifications_carry_the_views_inode_numbers() {
    let w = Workspace::new();
    let view = w.view(&["api", "web"]);
    let file = view.resolve("web/a.txt").unwrap();
    let told = recorded(&view);
    fs::write(w.project("web").join("a.txt"), "changed\n").unwrap();
    view.part("web")
        .unwrap()
        .project_changed(&["a.txt".to_string()]);
    let told = told.lock().unwrap().clone();
    assert!(!told.is_empty(), "the kernel is told");
    let web = view.lookup(ROOT, "web").unwrap().ino;
    assert!(
        told.iter().all(|c| match c {
            Change::Data(ino) => *ino == file,
            Change::Entry { parent, .. } => *parent == web,
        }),
        "in the view's numbers: {told:?} (file {file}, folder {web})"
    );
}

#[test]
fn changes_name_paths_and_origins_from_the_views_root() {
    let w = Workspace::new();
    let view = w.view(&["api", "web"]);
    let a = view.resolve("api/a.txt").unwrap();
    view.write(a, 0, b"API A\n").unwrap();
    let web = view.lookup(ROOT, "web").unwrap().ino;
    view.rename(web, "old.txt", web, "new.txt", RenameMode::Replace)
        .unwrap();
    let changes = view.changes();
    let names: Vec<(String, Option<String>)> = changes
        .iter()
        .map(|c| match c {
            OverlayChange::Write { path, origin, .. } => (path.clone(), origin.clone()),
            OverlayChange::Delete { path, .. } => (format!("-{path}"), None),
        })
        .collect();
    assert!(
        names.contains(&("api/a.txt".into(), Some("api/a.txt".into()))),
        "{names:?}"
    );
    assert!(
        names.contains(&("web/new.txt".into(), Some("web/old.txt".into()))),
        "a move within web: {names:?}"
    );
    // Its versions are found by the chat's path.
    assert!(view.versions("api/a.txt").is_some());
    assert_eq!(view.used().0, view.part("api").unwrap().used().0);
}

#[test]
fn a_folder_added_while_mounted_shows_and_old_inodes_stay_valid() {
    let w = Workspace::new();
    let view = w.view(&["api"]);
    let file = view.resolve("api/a.txt").unwrap();
    let before = view.generation();
    let told = recorded(&view);
    let left = w.matched(&view, &w.folders(&["api", "web"]));
    assert!(left.is_empty());
    assert_eq!(names_at_root(&view), ["api", "web"]);
    assert_eq!(view.getattr(file).unwrap().ino, file, "still api/a.txt");
    assert_eq!(view.path_of(file).unwrap(), "api/a.txt");
    assert_eq!(read(&view, "web/a.txt"), "web a\n");
    assert_eq!(
        *told.lock().unwrap(),
        [Change::Entry {
            parent: ROOT,
            name: "web".into()
        }]
    );
    assert_ne!(view.generation(), before);
}

#[test]
fn a_folder_that_left_is_dropped_from_the_root() {
    let w = Workspace::new();
    let view = w.view(&["api", "web"]);
    let file = view.resolve("web/a.txt").unwrap();
    let told = recorded(&view);
    let left = w.matched(&view, &w.folders(&["api"]));
    let names: Vec<&str> = left.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["web"], "for its store to go");
    assert_eq!(names_at_root(&view), ["api"]);
    assert_eq!(view.lookup(ROOT, "web"), Err(VfsError::NotFound));
    assert_eq!(view.getattr(file), Err(VfsError::NotFound));
    assert_eq!(
        *told.lock().unwrap(),
        [Change::Entry {
            parent: ROOT,
            name: "web".into()
        }]
    );
}

#[test]
fn a_folder_located_elsewhere_is_reopened_at_its_new_path() {
    let w = Workspace::new();
    let view = w.view(&["api", "web"]);
    let file = view.resolve("web/a.txt").unwrap();
    let elsewhere = w.dir.0.join("moved").join("web");
    fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
    fs::rename(w.project("web"), &elsewhere).unwrap();
    let left = w.matched(
        &view,
        &w.at(&[("api", w.project("api")), ("web", elsewhere.clone())]),
    );
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].1.base(), w.project("web").as_path());
    let found: Vec<(String, PathBuf)> = view.folders();
    assert_eq!(found[1], ("web".to_string(), elsewhere.clone()));
    assert_eq!(read(&view, "web/a.txt"), "web a\n");
    assert_eq!(
        view.getattr(file),
        Err(VfsError::NotFound),
        "the old place's numbers are the old part's"
    );
    assert!(Path::new(&elsewhere).join("old.txt").exists());
}

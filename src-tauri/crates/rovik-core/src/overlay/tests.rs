use super::fs::{Budget, OverlayChange, OverlayFs};
use crate::vfs::backend::Backend;
use crate::vfs::store::{Kind, RenameMode, SetAttr, VfsError, ROOT};
use std::fs;
use std::path::PathBuf;

struct Dirs {
    root: PathBuf,
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Dirs {
    fn project(&self) -> PathBuf {
        self.root.join("project")
    }
    fn store(&self) -> PathBuf {
        self.root.join("store")
    }
}

fn setup(files: &[(&str, &str)]) -> Dirs {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let root = std::env::temp_dir().join(format!(
        "overlay-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&root);
    let d = Dirs { root };
    fs::create_dir_all(d.project()).unwrap();
    for (path, content) in files {
        let p = d.project().join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }
    d
}

fn overlay(d: &Dirs) -> OverlayFs {
    OverlayFs::open(&d.project(), &d.store(), 1 << 30).unwrap()
}

fn ino(o: &OverlayFs, path: &str) -> u64 {
    o.resolve(path).unwrap()
}

fn read_all(o: &OverlayFs, path: &str) -> String {
    String::from_utf8(o.read(ino(o, path), 0, 1 << 20).unwrap()).unwrap()
}

fn write_all(o: &OverlayFs, path: &str, content: &str) {
    let i = ino(o, path);
    o.setattr(
        i,
        &SetAttr {
            size: Some(0),
            ..Default::default()
        },
    )
    .unwrap();
    o.write(i, 0, content.as_bytes()).unwrap();
}

fn paths(changes: &[OverlayChange]) -> Vec<String> {
    changes
        .iter()
        .map(|c| match c {
            OverlayChange::Delete { path, .. } => format!("-{path}"),
            OverlayChange::Write { path, .. } => format!("+{path}"),
        })
        .collect()
}

#[test]
fn untouched_files_read_through_and_writes_stay_in_the_overlay() {
    let d = setup(&[("src/a.txt", "one\n"), ("src/b.txt", "two\n")]);
    let o = overlay(&d);
    assert_eq!(read_all(&o, "src/a.txt"), "one\n");
    // The project underneath is live for what the chat hasn't changed.
    fs::write(d.project().join("src/b.txt"), "TWO\n").unwrap();
    assert_eq!(read_all(&o, "src/b.txt"), "TWO\n");

    write_all(&o, "src/a.txt", "changed\n");
    assert_eq!(read_all(&o, "src/a.txt"), "changed\n");
    assert_eq!(
        fs::read_to_string(d.project().join("src/a.txt")).unwrap(),
        "one\n"
    );
    let changes = o.changes();
    assert_eq!(paths(&changes), vec!["+src/a.txt"]);
    let OverlayChange::Write {
        base_copy: Some(base),
        upper: Some(upper),
        ..
    } = &changes[0]
    else {
        panic!()
    };
    assert_eq!(fs::read_to_string(base).unwrap(), "one\n");
    assert_eq!(fs::read_to_string(upper).unwrap(), "changed\n");
}

#[test]
fn creates_deletes_and_renames_are_listed_for_landing() {
    let d = setup(&[
        ("a.txt", "a"),
        ("dir/b.txt", "b"),
        ("dir/c.txt", "c"),
        ("keep.txt", "k"),
    ]);
    let o = overlay(&d);
    let f = o
        .create(ROOT, "new.txt", Kind::File, 0o644, 0, 0, None)
        .unwrap();
    o.write(f.ino, 0, b"new").unwrap();
    o.unlink(ROOT, "a.txt").unwrap();
    o.rename(
        ino(&o, "dir"),
        "b.txt",
        ROOT,
        "b-moved.txt",
        RenameMode::Replace,
    )
    .unwrap();
    assert_eq!(o.lookup(ROOT, "a.txt").unwrap_err(), VfsError::NotFound);
    assert_eq!(read_all(&o, "b-moved.txt"), "b");
    assert_eq!(
        paths(&o.changes()),
        vec!["-a.txt", "+b-moved.txt", "-dir/b.txt", "+new.txt"]
    );
    // Moved back: no change left there.
    o.rename(
        ROOT,
        "b-moved.txt",
        ino(&o, "dir"),
        "b.txt",
        RenameMode::Replace,
    )
    .unwrap();
    assert_eq!(paths(&o.changes()), vec!["-a.txt", "+new.txt"]);
}

#[test]
fn a_moved_folder_keeps_its_files_and_a_replaced_one_is_deleted_first() {
    let d = setup(&[
        ("old/x.txt", "x"),
        ("old/sub/y.txt", "y"),
        ("src/z.txt", "z"),
    ]);
    let o = overlay(&d);
    o.rename(ROOT, "old", ROOT, "new", RenameMode::Replace)
        .unwrap();
    assert_eq!(read_all(&o, "new/sub/y.txt"), "y");
    // `rm -rf src && mkdir src`: the project's src goes, the new one comes.
    let src = ino(&o, "src");
    o.unlink(src, "z.txt").unwrap();
    o.rmdir(ROOT, "src").unwrap();
    o.create(ROOT, "src", Kind::Dir, 0o755, 0, 0, None).unwrap();
    assert_eq!(
        paths(&o.changes()),
        vec![
            "-old",
            "-src",
            "+new",
            "+new/sub",
            "+new/sub/y.txt",
            "+new/x.txt",
            "+src"
        ]
    );
}

/// A folder the view listed, untouched, leaves it with all it holds when
/// it leaves the project; a file the project turned into a folder (or the
/// other way) is seen as it is now. One the chat changed inside stays.
#[test]
fn a_listed_folder_that_left_the_project_leaves_the_view_and_kinds_follow_it() {
    let d = setup(&[("gen/a.txt", "a"), ("keep/b.txt", "b"), ("x", "file")]);
    let o = overlay(&d);
    o.readdir_plus(ROOT).unwrap();
    o.readdir_plus(ino(&o, "gen")).unwrap();
    o.readdir_plus(ino(&o, "keep")).unwrap();
    write_all(&o, "keep/b.txt", "the chat's");
    fs::remove_dir_all(d.project().join("gen")).unwrap();
    fs::remove_dir_all(d.project().join("keep")).unwrap();
    fs::remove_file(d.project().join("x")).unwrap();
    fs::create_dir_all(d.project().join("x")).unwrap();
    fs::write(d.project().join("x/inside.txt"), "i").unwrap();
    o.project_changed_everywhere();
    let mut names: Vec<String> = o
        .readdir_plus(ROOT)
        .unwrap()
        .into_iter()
        .map(|(e, _)| e.name)
        .collect();
    names.sort();
    assert_eq!(names, ["keep", "x"]);
    assert_eq!(read_all(&o, "keep/b.txt"), "the chat's");
    assert_eq!(read_all(&o, "x/inside.txt"), "i");
}

/// On Unix a backslash is part of a name: a file named `a\b` is that
/// file, found again by its path after the overlay reopens, not `a/b`.
#[cfg(unix)]
#[test]
fn a_backslash_in_a_name_is_part_of_the_name() {
    let d = setup(&[("a/b", "the folder's")]);
    {
        let o = overlay(&d);
        let f = o
            .create(ROOT, "a\\b", Kind::File, 0o644, 0, 0, None)
            .unwrap();
        o.write(f.ino, 0, b"the backslash's").unwrap();
        o.flush();
    }
    let o = overlay(&d);
    assert_eq!(read_all(&o, "a\\b"), "the backslash's");
    assert_eq!(read_all(&o, "a/b"), "the folder's");
}

#[test]
fn files_appearing_in_the_project_show_up_unless_the_chat_removed_them() {
    let d = setup(&[("a.txt", "a")]);
    let o = overlay(&d);
    o.readdir_plus(ROOT).unwrap();
    o.unlink(ROOT, "a.txt").unwrap();
    fs::write(d.project().join("later.txt"), "l").unwrap();
    // What the project's watcher (or a landing) tells the overlay.
    o.project_changed(&["later.txt".to_string()]);
    let names: Vec<String> = o
        .readdir_plus(ROOT)
        .unwrap()
        .into_iter()
        .map(|(e, _)| e.name)
        .collect();
    assert_eq!(names, vec!["later.txt"]);
    // Untouched entries that leave the project leave the view too.
    fs::remove_file(d.project().join("later.txt")).unwrap();
    o.project_changed(&["later.txt".to_string()]);
    assert!(o.readdir_plus(ROOT).unwrap().is_empty());
}

#[test]
fn the_limit_counts_changes_but_not_build_output() {
    let d = setup(&[(".gitignore", "/target\n"), ("src/a.txt", "")]);
    let o = OverlayFs::open(&d.project(), &d.store(), 1000).unwrap();
    let warned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let w = warned.clone();
    o.on_warn(Box::new(move |_, _| {
        w.store(true, std::sync::atomic::Ordering::Relaxed)
    }));
    let a = ino(&o, "src/a.txt");
    o.write(a, 0, &[b'x'; 700]).unwrap();
    assert!(!warned.load(std::sync::atomic::Ordering::Relaxed));
    o.write(a, 700, &[b'x'; 150]).unwrap();
    assert!(warned.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(
        o.write(a, 850, &[b'x'; 200]).unwrap_err(),
        VfsError::NoSpace
    );
    // Build output doesn't count.
    let target = o
        .create(ROOT, "target", Kind::Dir, 0o755, 0, 0, None)
        .unwrap();
    let big = o
        .create(target.ino, "app", Kind::File, 0o755, 0, 0, None)
        .unwrap();
    o.write(big.ino, 0, &[0; 5000]).unwrap();
    assert_eq!(o.used().0, 850);
    let OverlayChange::Write { by_product, .. } = o
        .changes()
        .into_iter()
        .find(|c| c.path() == "target/app")
        .unwrap()
    else {
        panic!()
    };
    assert!(by_product);
}

/// Two folders of one workspace chat, each with its file `a.txt`, sharing
/// one budget of `limit` bytes (workspaces-plan.md, 4.2).
fn two_parts(limit: u64) -> (Dirs, Dirs, OverlayFs, OverlayFs) {
    let (one, two) = (setup(&[("a.txt", "")]), setup(&[("a.txt", "")]));
    let budget = Budget::new(limit);
    let api = OverlayFs::open_in(&one.project(), &one.store(), budget.clone()).unwrap();
    let web = OverlayFs::open_in(&two.project(), &two.store(), budget).unwrap();
    (one, two, api, web)
}

#[test]
fn parts_of_one_chat_share_its_limit() {
    let (_one, _two, api, web) = two_parts(1000);
    api.write(ino(&api, "a.txt"), 0, &[b'x'; 600]).unwrap();
    assert_eq!(
        web.write(ino(&web, "a.txt"), 0, &[b'x'; 600]).unwrap_err(),
        VfsError::NoSpace,
        "the chat's room, not each folder's"
    );
    web.write(ino(&web, "a.txt"), 0, &[b'x'; 400]).unwrap();
    assert_eq!(api.used(), (1000, 1000));
    assert_eq!(web.used(), (1000, 1000));
}

#[test]
fn two_parts_writing_at_once_cant_both_pass_the_limit() {
    for _ in 0..20 {
        let (_one, _two, api, web) = two_parts(1000);
        let start = std::sync::Barrier::new(2);
        let (start, parts) = (&start, [&api, &web]);
        let (first, second) = std::thread::scope(|s| {
            let write = move |part: &OverlayFs| {
                let at = ino(part, "a.txt");
                start.wait();
                part.write(at, 0, &[b'x'; 600])
            };
            let first = s.spawn(move || write(parts[0]));
            let second = s.spawn(move || write(parts[1]));
            (first.join().unwrap(), second.join().unwrap())
        });
        assert_eq!(
            [first.is_ok(), second.is_ok()]
                .iter()
                .filter(|ok| **ok)
                .count(),
            1,
            "one of them fits: {first:?}, {second:?}"
        );
        assert_eq!(api.used().0, 600);
    }
}

#[test]
fn the_warning_comes_once_per_chat_from_whichever_part_reaches_80_percent() {
    let (_one, _two, api, web) = two_parts(1000);
    let warned = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let w = warned.clone();
    api.on_warn(Box::new(move |used, limit| {
        w.lock().unwrap().push((used, limit))
    }));
    api.write(ino(&api, "a.txt"), 0, &[b'x'; 500]).unwrap();
    assert!(warned.lock().unwrap().is_empty());
    web.write(ino(&web, "a.txt"), 0, &[b'x'; 350]).unwrap();
    assert_eq!(
        *warned.lock().unwrap(),
        [(850, 1000)],
        "web's write reached it"
    );
    api.write(ino(&api, "a.txt"), 500, &[b'x'; 50]).unwrap();
    assert_eq!(warned.lock().unwrap().len(), 1, "once per chat");
}

#[test]
fn a_part_that_goes_gives_its_share_back() {
    let (_one, _two, api, web) = two_parts(1000);
    api.write(ino(&api, "a.txt"), 0, &[b'x'; 300]).unwrap();
    web.write(ino(&web, "a.txt"), 0, &[b'x'; 200]).unwrap();
    drop(web);
    assert_eq!(api.used().0, 300, "a folder that left isn't counted");
}

#[test]
fn an_overlay_reopens_as_it_was() {
    let d = setup(&[("a.txt", "a"), ("dir/b.txt", "b"), ("gone.txt", "g")]);
    {
        let o = overlay(&d);
        write_all(&o, "a.txt", "A");
        o.unlink(ROOT, "gone.txt").unwrap();
        o.rename(ROOT, "dir", ROOT, "dir2", RenameMode::Replace)
            .unwrap();
        let n = o
            .create(ROOT, "n.txt", Kind::File, 0o600, 0, 0, None)
            .unwrap();
        o.write(n.ino, 0, b"N").unwrap();
        o.setattr(
            ino(&o, "dir2/b.txt"),
            &SetAttr {
                perm: Some(0o755),
                ..Default::default()
            },
        )
        .unwrap();
        o.flush();
    }
    let o = overlay(&d);
    assert_eq!(read_all(&o, "a.txt"), "A");
    assert_eq!(read_all(&o, "n.txt"), "N");
    assert_eq!(read_all(&o, "dir2/b.txt"), "b");
    assert_eq!(o.getattr(ino(&o, "dir2/b.txt")).unwrap().perm, 0o755);
    assert_eq!(o.lookup(ROOT, "gone.txt").unwrap_err(), VfsError::NotFound);
    assert_eq!(o.lookup(ROOT, "dir").unwrap_err(), VfsError::NotFound);
    assert_eq!(
        paths(&o.changes()),
        vec![
            "-dir",
            "-gone.txt",
            "+a.txt",
            "+dir2",
            "+dir2/b.txt",
            "+n.txt"
        ]
    );
}

#[test]
fn renames_follow_posix_rules() {
    let d = setup(&[
        ("f.txt", "f"),
        ("g.txt", "g"),
        ("d/x", "x"),
        ("e/.keep", ""),
    ]);
    let o = overlay(&d);
    assert_eq!(
        o.rename(ROOT, "f.txt", ROOT, "g.txt", RenameMode::NoReplace)
            .unwrap_err(),
        VfsError::Exists
    );
    o.rename(ROOT, "f.txt", ROOT, "g.txt", RenameMode::Exchange)
        .unwrap();
    assert_eq!(read_all(&o, "g.txt"), "f");
    assert_eq!(read_all(&o, "f.txt"), "g");
    assert_eq!(
        o.rename(ROOT, "d", ino(&o, "d"), "inside", RenameMode::Replace)
            .unwrap_err(),
        VfsError::Invalid
    );
    assert_eq!(
        o.rename(ROOT, "f.txt", ROOT, "d", RenameMode::Replace)
            .unwrap_err(),
        VfsError::IsDir
    );
    assert_eq!(
        o.rename(ROOT, "e", ROOT, "d", RenameMode::Replace)
            .unwrap_err(),
        VfsError::NotEmpty
    );
    assert_eq!(o.rmdir(ROOT, "d").unwrap_err(), VfsError::NotEmpty);
    // A hard link shares the content; removing one name keeps the other.
    let f = ino(&o, "f.txt");
    o.link(f, ROOT, "f2.txt").unwrap();
    o.write(f, 0, b"G2").unwrap();
    assert_eq!(read_all(&o, "f2.txt"), "G2");
    o.unlink(ROOT, "f.txt").unwrap();
    assert_eq!(read_all(&o, "f2.txt"), "G2");
}

/// Two hard links in one folder: removing or moving one name acts on that
/// name, not on the one that sorts first (`ln a z; rm z` kept `z`).
#[test]
fn removing_or_moving_one_of_two_links_in_a_folder_acts_on_that_name() {
    let d = setup(&[("a.txt", "A")]);
    let o = overlay(&d);
    let a = ino(&o, "a.txt");
    o.link(a, ROOT, "z.txt").unwrap();
    o.unlink(ROOT, "z.txt").unwrap();
    assert_eq!(o.lookup(ROOT, "z.txt").unwrap_err(), VfsError::NotFound);
    assert_eq!(read_all(&o, "a.txt"), "A");

    o.link(a, ROOT, "z.txt").unwrap();
    o.rename(ROOT, "z.txt", ROOT, "moved.txt", RenameMode::Replace)
        .unwrap();
    assert_eq!(o.lookup(ROOT, "z.txt").unwrap_err(), VfsError::NotFound);
    assert_eq!(read_all(&o, "a.txt"), "A");
    assert_eq!(read_all(&o, "moved.txt"), "A");
}

#[test]
fn a_file_unlinked_while_open_stays_readable_until_released() {
    let d = setup(&[]);
    let o = overlay(&d);
    let f = o
        .create(ROOT, "tmp", Kind::File, 0o644, 0, 0, None)
        .unwrap();
    o.write(f.ino, 0, b"data").unwrap();
    o.open(f.ino, false).unwrap();
    o.unlink(ROOT, "tmp").unwrap();
    assert_eq!(o.read(f.ino, 0, 10).unwrap(), b"data");
    o.release(f.ino);
    assert_eq!(o.read(f.ino, 0, 10).unwrap_err(), VfsError::NotFound);
    assert!(o.changes().is_empty());
}

/// Through a real mount: ordinary programs and random operations, checked
/// against the same operations on a plain copy of the project.
#[cfg(target_os = "linux")]
/// A change in a project folder that's gone (a branch without it): the
/// saved list can't place it now, so it's kept as it is and its file is
/// named to the sweep, and it's back once the folder is.
#[test]
fn a_change_whose_folder_the_project_lost_waits_for_it() {
    let d = setup(&[("src/a.txt", "a")]);
    {
        let o = overlay(&d);
        let src = ino(&o, "src");
        let n = o
            .create(src, "new.txt", Kind::File, 0o644, 0, 0, None)
            .unwrap();
        o.write(n.ino, 0, b"kept").unwrap();
        o.flush();
    }
    fs::remove_dir_all(d.project().join("src")).unwrap();
    {
        let o = overlay(&d);
        o.sweep_orphans();
        assert_eq!(paths(&o.changes()), vec!["+src/new.txt"]);
        o.flush();
    }
    let files = fs::read_dir(d.store().join("upper")).unwrap().count();
    assert_eq!(files, 1, "its file stays");
    fs::create_dir_all(d.project().join("src")).unwrap();
    let o = overlay(&d);
    assert_eq!(read_all(&o, "src/new.txt"), "kept");
}

/// A saved list that can't be read (a newer Rovik's, or a broken file)
/// doesn't open as an empty one: opened so, the sweep would delete every
/// file it names.
#[test]
fn a_saved_list_that_cant_be_read_doesnt_open_as_empty() {
    let d = setup(&[("a.txt", "a")]);
    {
        let o = overlay(&d);
        write_all(&o, "a.txt", "A");
        o.flush();
    }
    fs::write(d.store().join("manifest.json"), b"{\"version\": 2").unwrap();
    let opened = OverlayFs::open(&d.project(), &d.store(), 1 << 30);
    assert!(opened.is_err());
    let files = fs::read_dir(d.store().join("upper")).unwrap().count();
    assert_eq!(files, 1, "its file stays");
}

/// A hard-linked file is saved once per name, as one file: opened again,
/// its names share it still, and unlinking one leaves it under the other.
#[test]
fn a_hard_links_names_share_their_file_after_reopening() {
    let d = setup(&[]);
    {
        let o = overlay(&d);
        let n = o
            .create(ROOT, "n.txt", Kind::File, 0o644, 0, 0, None)
            .unwrap();
        o.write(n.ino, 0, b"N").unwrap();
        o.link(n.ino, ROOT, "m.txt").unwrap();
        o.flush();
    }
    {
        let o = overlay(&d);
        assert_eq!(ino(&o, "n.txt"), ino(&o, "m.txt"), "one file");
        o.unlink(ROOT, "m.txt").unwrap();
        assert_eq!(read_all(&o, "n.txt"), "N");
        o.flush();
    }
    let o = overlay(&d);
    o.sweep_orphans();
    assert_eq!(read_all(&o, "n.txt"), "N");
}

/// A saved path that isn't the chat's own (a list written by hand) is
/// left out, and never lands: `..` doesn't reach outside the project.
#[test]
fn a_saved_path_outside_the_project_is_left_out() {
    let d = setup(&[("a.txt", "a")]);
    let upper = d.store().join("upper");
    fs::create_dir_all(&upper).unwrap();
    fs::write(upper.join("1"), b"evil").unwrap();
    let outside = OverlayChange::Write {
        path: "../evil.txt".into(),
        kind: Kind::File,
        origin: None,
        upper: Some(upper.join("1")),
        base_copy: None,
        perm: 0o644,
        perm_changed: false,
        target: None,
        by_product: false,
    };
    super::fs::write_manifest(&d.store(), 2, vec![outside.clone()]).unwrap();
    let o = overlay(&d);
    assert!(o.changes().is_empty(), "{:?}", o.changes());
    let plan = super::land::plan(&d.project(), &[outside], false);
    let reasons: Vec<&str> = plan.conflicts.iter().map(|c| c.reason.as_str()).collect();
    assert_eq!(reasons, ["it isn't a path inside the project"]);
}

/// A saved change whose files can't be this overlay's (a list written by
/// hand) is left out too: its content from outside a store's folders, or
/// the project's file from outside the project, never lands.
#[test]
fn a_saved_change_naming_files_outside_its_store_or_project_is_left_out() {
    let d = setup(&[("a.txt", "a")]);
    let secret = d.root.join("secret.txt");
    fs::write(&secret, b"not the chat's").unwrap();
    let upper = d.store().join("upper");
    fs::create_dir_all(&upper).unwrap();
    fs::write(upper.join("1"), b"the chat's").unwrap();
    let write =
        |origin: Option<&str>, upper: PathBuf, base_copy: Option<PathBuf>| OverlayChange::Write {
            path: "b.txt".into(),
            kind: Kind::File,
            origin: origin.map(str::to_string),
            upper: Some(upper),
            base_copy,
            perm: 0o644,
            perm_changed: false,
            target: None,
            by_product: false,
        };
    for bad in [
        write(None, secret.clone(), None),
        write(None, upper.join("1"), Some(secret.clone())),
        write(Some("../secret.txt"), upper.join("1"), None),
        write(None, d.store().join("upper/../../secret.txt"), None),
    ] {
        super::fs::write_manifest(&d.store(), 2, vec![bad.clone()]).unwrap();
        let o = overlay(&d);
        assert!(o.changes().is_empty(), "{bad:?}: {:?}", o.changes());
    }
    // This store's own file is fine; so is one saved where the app's data
    // folder was before it moved: it's this store's by folder and name.
    for own in [upper.join("1"), PathBuf::from("/moved/data/store/upper/1")] {
        super::fs::write_manifest(&d.store(), 2, vec![write(None, own, None)]).unwrap();
        assert_eq!(read_all(&overlay(&d), "b.txt"), "the chat's");
    }
}

mod mounted {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;

    fn mount(d: &Dirs) -> Option<(crate::vfs::Mount, PathBuf, Arc<OverlayFs>)> {
        let o = Arc::new(overlay(d));
        let at = d.root.join("view");
        match crate::vfs::platform_mount(o.clone(), Some(&at)) {
            Ok((m, at)) => Some((m, at, o)),
            Err(e) => {
                eprintln!("no FUSE here, skipping: {e:?}");
                None
            }
        }
    }

    fn sh(dir: &Path, cmd: &str) {
        let ok = std::process::Command::new("sh")
            .args(["-c", cmd])
            .current_dir(dir)
            .status()
            .unwrap()
            .success();
        assert!(ok, "{cmd}");
    }

    #[test]
    // Needs FUSE: skipped where mounts don't work.
    fn common_tools_change_only_the_overlay() {
        if !crate::vfs::mount_works() {
            return;
        }
        let d = setup(&[
            ("src/main.rs", "fn main() {}\n"),
            ("README.md", "hi\n"),
            ("old/x", "x\n"),
        ]);
        let Some((m, view, o)) = mount(&d) else {
            return;
        };
        sh(&view, "sed -i 's/main/start/' src/main.rs");
        sh(
            &view,
            "echo more >> README.md && mkdir -p docs/a && echo doc > docs/a/b.md",
        );
        sh(
            &view,
            "mv old renamed && rm README.md && chmod +x src/main.rs && ln -s src/main.rs link",
        );
        sh(&view, "test \"$(cat src/main.rs)\" = 'fn start() {}' && test -x src/main.rs && test \"$(cat link)\" = 'fn start() {}'");
        // The project is untouched.
        assert_eq!(
            fs::read_to_string(d.project().join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
        assert!(d.project().join("README.md").exists());
        assert!(d.project().join("old/x").exists());
        let mut p = paths(&o.changes());
        p.sort();
        assert_eq!(
            p,
            vec![
                "+docs",
                "+docs/a",
                "+docs/a/b.md",
                "+link",
                "+renamed",
                "+renamed/x",
                "+src/main.rs",
                "-README.md",
                "-old"
            ]
        );
        // `sed -i` saved through a temporary file: still the same file, with
        // what it was before as the base to merge against.
        let main = o
            .changes()
            .into_iter()
            .find(|c| c.path() == "src/main.rs")
            .unwrap();
        let OverlayChange::Write {
            origin,
            base_copy: Some(base),
            ..
        } = main
        else {
            panic!("{main:?}")
        };
        assert_eq!(origin.as_deref(), Some("src/main.rs"));
        assert_eq!(fs::read_to_string(base).unwrap(), "fn main() {}\n");
        m.unmount().unwrap();
    }

    #[test]
    // Needs FUSE: skipped where mounts don't work.
    fn random_operations_match_a_plain_copy() {
        if !crate::vfs::mount_works() {
            return;
        }
        let files: Vec<(String, String)> = (0..6)
            .map(|i| (format!("d{}/f{i}.txt", i % 3), format!("content {i}\n")))
            .collect();
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let d = setup(&refs);
        let copy = d.root.join("copy");
        sh(&d.root, "cp -r project copy");
        let Some((m, view, _o)) = mount(&d) else {
            return;
        };
        let mut seed: u64 = 0x2545F4914F6CDD1D;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for step in 0..300 {
            let a = format!("d{}/f{}.txt", next(4), next(8));
            let b = format!("d{}/f{}.txt", next(4), next(8));
            let op = next(6);
            let apply = |root: &Path| -> bool {
                let (pa, pb) = (root.join(&a), root.join(&b));
                match op {
                    0 => fs::write(&pa, format!("step {step}\n")).is_ok(),
                    1 => fs::OpenOptions::new()
                        .append(true)
                        .open(&pa)
                        .and_then(|mut f| std::io::Write::write_all(&mut f, b"+"))
                        .is_ok(),
                    2 => fs::remove_file(&pa).is_ok(),
                    3 => fs::rename(&pa, &pb).is_ok(),
                    4 => fs::create_dir_all(pa.parent().unwrap()).is_ok(),
                    _ => fs::remove_dir(pa.parent().unwrap()).is_ok(),
                }
            };
            let (x, y) = (apply(&copy), apply(&view));
            assert_eq!(x, y, "step {step}: op {op} on {a} / {b}");
        }
        assert_eq!(snapshot(&copy), snapshot(&view));
        m.unmount().unwrap();
    }

    fn snapshot(root: &Path) -> Vec<(String, Option<String>)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for e in fs::read_dir(&dir).unwrap() {
                let e = e.unwrap();
                let rel = e
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if e.file_type().unwrap().is_dir() {
                    out.push((rel, None));
                    stack.push(e.path());
                } else {
                    out.push((rel, Some(fs::read_to_string(e.path()).unwrap())));
                }
            }
        }
        out.sort();
        out
    }
}

mod landing {
    use super::*;
    use crate::overlay::land::{self, LandError};

    fn data(d: &Dirs) -> PathBuf {
        d.root.join("data")
    }

    fn landable(o: &OverlayFs) -> Vec<OverlayChange> {
        o.changes()
            .into_iter()
            .filter(|c| !c.by_product())
            .collect()
    }

    fn project_file(d: &Dirs, rel: &str) -> Option<String> {
        fs::read_to_string(d.project().join(rel)).ok()
    }

    #[test]
    fn changes_land_and_undo_puts_everything_back() {
        let d = setup(&[
            ("a.txt", "one\n"),
            ("old/x.txt", "x\n"),
            ("gone.txt", "g\n"),
        ]);
        let o = overlay(&d);
        write_all(&o, "a.txt", "ONE\n");
        o.unlink(ROOT, "gone.txt").unwrap();
        o.rename(ROOT, "old", ROOT, "new", RenameMode::Replace)
            .unwrap();
        let n = o
            .create(ROOT, "n.txt", Kind::File, 0o644, 0, 0, None)
            .unwrap();
        o.write(n.ino, 0, b"N\n").unwrap();

        let landed = land::land(&data(&d), "chat", &d.project(), &landable(&o), false)
            .unwrap()
            .unwrap();
        assert_eq!(project_file(&d, "a.txt").as_deref(), Some("ONE\n"));
        assert_eq!(project_file(&d, "n.txt").as_deref(), Some("N\n"));
        assert_eq!(project_file(&d, "new/x.txt").as_deref(), Some("x\n"));
        assert!(project_file(&d, "gone.txt").is_none());
        assert!(!d.project().join("old").exists());

        let (id, _, _) = land::latest(&data(&d), "chat").unwrap();
        assert_eq!(id, landed.id);
        let kept = land::undo(&data(&d), "chat", &d.project(), &id).unwrap();
        assert!(kept.is_empty(), "{kept:?}");
        assert_eq!(project_file(&d, "a.txt").as_deref(), Some("one\n"));
        assert_eq!(project_file(&d, "old/x.txt").as_deref(), Some("x\n"));
        assert_eq!(project_file(&d, "gone.txt").as_deref(), Some("g\n"));
        assert!(project_file(&d, "n.txt").is_none());
        assert!(!d.project().join("new").exists());
        assert!(
            land::latest(&data(&d), "chat").is_none(),
            "undone landings can't be undone again"
        );
    }

    #[test]
    fn changes_merge_with_the_projects_own() {
        let d = setup(&[("a.txt", "1\n2\n3\n4\n5\n")]);
        let o = overlay(&d);
        write_all(&o, "a.txt", "ONE\n2\n3\n4\n5\n");
        // Meanwhile, in the project:
        fs::write(d.project().join("a.txt"), "1\n2\n3\n4\nFIVE\n").unwrap();
        let plan = land::plan(&d.project(), &landable(&o), false);
        assert!(plan.merged && plan.conflicts.is_empty());
        land::land(&data(&d), "chat", &d.project(), &landable(&o), false).unwrap();
        assert_eq!(
            project_file(&d, "a.txt").as_deref(),
            Some("ONE\n2\n3\n4\nFIVE\n")
        );
    }

    #[test]
    fn the_same_lines_changed_twice_stop_the_landing() {
        let d = setup(&[("a.txt", "1\n2\n"), ("b.txt", "b\n")]);
        let o = overlay(&d);
        write_all(&o, "a.txt", "chat\n2\n");
        write_all(&o, "b.txt", "B\n");
        fs::write(d.project().join("a.txt"), "project\n2\n").unwrap();
        let Err(LandError::Conflicts(c)) =
            land::land(&data(&d), "chat", &d.project(), &landable(&o), false)
        else {
            panic!("expected a conflict")
        };
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].path, "a.txt");
        // Nothing landed, not even the file without a conflict.
        assert_eq!(project_file(&d, "b.txt").as_deref(), Some("b\n"));
        // Land anyway: the chat's version wins.
        land::land(&data(&d), "chat", &d.project(), &landable(&o), true).unwrap();
        assert_eq!(project_file(&d, "a.txt").as_deref(), Some("chat\n2\n"));
        assert_eq!(project_file(&d, "b.txt").as_deref(), Some("B\n"));
    }

    #[test]
    fn a_file_deleted_in_the_project_but_changed_in_the_chat_conflicts() {
        let d = setup(&[("a.txt", "a\n")]);
        let o = overlay(&d);
        write_all(&o, "a.txt", "A\n");
        fs::remove_file(d.project().join("a.txt")).unwrap();
        let plan = land::plan(&d.project(), &landable(&o), false);
        assert_eq!(plan.conflicts.len(), 1);
    }

    #[test]
    fn nothing_to_land_when_the_project_already_has_it() {
        let d = setup(&[("a.txt", "a\n")]);
        let o = overlay(&d);
        write_all(&o, "a.txt", "A\n");
        fs::write(d.project().join("a.txt"), "A\n").unwrap();
        assert!(
            land::land(&data(&d), "chat", &d.project(), &landable(&o), false)
                .unwrap()
                .is_none()
        );
    }

    /// Round 2 (section 4.2): after a restart, landings into a chat's view
    /// (children's, marked with their parent) are left for that view; of
    /// them only an "applying" one is finished, and only there.
    #[test]
    fn app_restart_with_a_journal_in_each_state() {
        let d = setup(&[
            ("a.txt", "a\n"),
            ("b.txt", "b\n"),
            ("c.txt", "c\n"),
            ("e.txt", "e\n"),
        ]);
        let o = overlay(&d);
        for f in ["a", "b", "c", "e"] {
            write_all(&o, &format!("{f}.txt"), &format!("{f} landed\n"));
        }
        let all = landable(&o);
        let states = [
            ("a", "applying"),
            ("b", "done"),
            ("c", "failed"),
            ("e", "partial"),
        ];
        for (f, state) in states {
            let child = format!("kid-{f}");
            let one: Vec<_> = all
                .iter()
                .filter(|c| c.path() == format!("{f}.txt"))
                .cloned()
                .collect();
            land::mark_into_chat(&data(&d), &child, "parent").unwrap();
            land::land(&data(&d), &child, &d.project(), &one, false).unwrap();
            let (id, _, _) = land::latest(&data(&d), &child).unwrap();
            let journal = data(&d)
                .join("landings")
                .join(&child)
                .join(&id)
                .join("journal.json");
            let text = fs::read_to_string(&journal)
                .unwrap()
                .replace("\"done\"", &format!("\"{state}\""));
            fs::write(&journal, text).unwrap();
            // As if each stopped before writing its file.
            fs::write(d.project().join(format!("{f}.txt")), format!("{f}\n")).unwrap();
        }
        // At start, nothing of a chat's children: its view isn't mounted.
        assert!(land::recover(&data(&d)).is_empty());
        for f in ["a", "b", "c", "e"] {
            assert_eq!(
                project_file(&d, &format!("{f}.txt")),
                Some(format!("{f}\n"))
            );
        }
        assert!(land::interrupted_into(&data(&d), "parent", &d.project()));
        assert!(!land::interrupted_into(&data(&d), "another", &d.project()));
        // With the view: only the applying one is finished.
        let done = land::recover_into(&data(&d), "parent", &d.project());
        assert_eq!(done, ["a.txt"]);
        assert_eq!(project_file(&d, "a.txt").as_deref(), Some("a landed\n"));
        for f in ["b", "c", "e"] {
            assert_eq!(
                project_file(&d, &format!("{f}.txt")),
                Some(format!("{f}\n"))
            );
        }
        assert!(!land::interrupted_into(&data(&d), "parent", &d.project()));
    }

    /// Round 2 (section 4.2): a landing recorded before into-chat.txt
    /// existed is found by the view's path.
    #[test]
    fn a_legacy_landing_without_into_chat_is_still_found_by_the_views_path() {
        let d = setup(&[("a.txt", "a\n")]);
        let o = overlay(&d);
        write_all(&o, "a.txt", "a landed\n");
        land::land(&data(&d), "old-kid", &d.project(), &landable(&o), false).unwrap();
        let (id, _, _) = land::latest(&data(&d), "old-kid").unwrap();
        let journal = data(&d)
            .join("landings")
            .join("old-kid")
            .join(&id)
            .join("journal.json");
        let text = fs::read_to_string(&journal)
            .unwrap()
            .replace("\"done\"", "\"applying\"");
        fs::write(&journal, text).unwrap();
        fs::write(d.project().join("a.txt"), "a\n").unwrap();
        assert!(land::interrupted_into(
            &data(&d),
            "any-parent",
            &d.project()
        ));
        assert_eq!(
            land::recover_into(&data(&d), "any-parent", &d.project()),
            ["a.txt"]
        );
        assert_eq!(project_file(&d, "a.txt").as_deref(), Some("a landed\n"));
    }

    #[test]
    fn an_interrupted_landing_is_finished_at_start() {
        let d = setup(&[("a.txt", "one\n")]);
        let o = overlay(&d);
        write_all(&o, "a.txt", "ONE\n");
        land::land(&data(&d), "chat", &d.project(), &landable(&o), false).unwrap();
        // As if the app died after writing the journal, before the files.
        fs::write(d.project().join("a.txt"), "one\n").unwrap();
        let (id, _, _) = land::latest(&data(&d), "chat").unwrap();
        let journal = data(&d)
            .join("landings")
            .join("chat")
            .join(&id)
            .join("journal.json");
        let text = fs::read_to_string(&journal)
            .unwrap()
            .replace("\"done\"", "\"applying\"");
        fs::write(&journal, text).unwrap();
        let done = land::recover(&data(&d));
        assert_eq!(done, vec!["a.txt"]);
        assert_eq!(project_file(&d, "a.txt").as_deref(), Some("ONE\n"));
    }

    /// Another of Rovik's processes, starting while one writes a landing,
    /// finds its journal "applying": not one a crash cut, and not its own
    /// to finish while that one holds it.
    #[test]
    fn a_landing_another_process_is_writing_isnt_finished_at_its_start() {
        let d = setup(&[("a.txt", "one\n")]);
        let o = overlay(&d);
        write_all(&o, "a.txt", "ONE\n");
        land::land(&data(&d), "chat", &d.project(), &landable(&o), false).unwrap();
        fs::write(d.project().join("a.txt"), "one\n").unwrap();
        let (id, _, _) = land::latest(&data(&d), "chat").unwrap();
        let dir = data(&d).join("landings").join("chat").join(&id);
        let text = fs::read_to_string(dir.join("journal.json"))
            .unwrap()
            .replace("\"done\"", "\"applying\"");
        fs::write(dir.join("journal.json"), text).unwrap();
        let writing = crate::os_lock::try_hold(&dir.join(land::APPLYING))
            .unwrap()
            .unwrap();
        assert!(land::recover(&data(&d)).is_empty());
        assert_eq!(project_file(&d, "a.txt").as_deref(), Some("one\n"));
        // That process crashed: it's interrupted now.
        drop(writing);
        assert_eq!(land::recover(&data(&d)), vec!["a.txt"]);
        assert_eq!(project_file(&d, "a.txt").as_deref(), Some("ONE\n"));
    }
}

#[test]
fn the_projects_changes_merged_in_land_as_they_are() {
    let d = setup(&[("a.txt", "1\n2\n3\n4\n5\n")]);
    let o = overlay(&d);
    write_all(&o, "a.txt", "ONE\n2\n3\n4\n5\n");
    fs::write(d.project().join("a.txt"), "1\n2\n3\n4\nFIVE\n").unwrap();
    let plan = super::land::plan(&d.project(), &o.changes(), false);
    assert!(plan.merged);
    let files = plan.merged_files();
    assert_eq!(files.len(), 1);
    for (path, merged, project) in &files {
        o.rebase_file(path, Some(merged), Some(project)).unwrap();
    }
    // The chat's view has both changes; landing now just takes it.
    assert_eq!(read_all(&o, "a.txt"), "ONE\n2\n3\n4\nFIVE\n");
    let again = super::land::plan(&d.project(), &o.changes(), false);
    assert!(!again.merged && again.conflicts.is_empty());
    assert_eq!(again.paths(), vec!["a.txt"]);
}

#[test]
fn a_rebased_file_survives_reopening() {
    let d = setup(&[("a.txt", "1\n2\n3\n"), ("b.txt", "b\n")]);
    {
        let o = overlay(&d);
        write_all(&o, "a.txt", "A\n2\n3\n");
        write_all(&o, "b.txt", "chat\n");
        fs::write(d.project().join("a.txt"), "P\n2\n3\n").unwrap();
        o.rebase_file("a.txt", Some(b"PA\n2\n3\n"), Some(b"P\n2\n3\n"))
            .unwrap();
        // Resolved by deleting it: gone from the view, and a delete lands.
        o.rebase_file("b.txt", None, Some(b"b\n")).unwrap();
        o.flush();
    }
    let o = overlay(&d);
    assert_eq!(read_all(&o, "a.txt"), "PA\n2\n3\n");
    assert_eq!(o.resolve("b.txt"), Err(VfsError::NotFound));
    let plan = super::land::plan(&d.project(), &o.changes(), false);
    assert!(plan.conflicts.is_empty(), "{:?}", plan.conflicts);
    let mut p = plan.paths();
    p.sort();
    assert_eq!(p, vec!["a.txt", "b.txt"]);
}

/// Nothing is lost: deletes, moves, failed landings, crashes, undo.
mod safety {
    use super::*;
    use crate::overlay::land::{self, LandError};

    fn data(d: &Dirs) -> PathBuf {
        d.root.join("data")
    }

    fn landable(o: &OverlayFs) -> Vec<OverlayChange> {
        o.changes()
            .into_iter()
            .filter(|c| !c.by_product())
            .collect()
    }

    fn project_file(d: &Dirs, rel: &str) -> Option<String> {
        fs::read_to_string(d.project().join(rel)).ok()
    }

    #[test]
    fn a_delete_of_a_file_changed_in_the_project_conflicts() {
        let d = setup(&[("a.txt", "one\n"), ("b.txt", "b\n")]);
        let o = overlay(&d);
        o.unlink(ROOT, "a.txt").unwrap();
        o.unlink(ROOT, "b.txt").unwrap();
        fs::write(d.project().join("a.txt"), "user's edit\n").unwrap();
        let plan = land::plan(&d.project(), &landable(&o), false);
        assert_eq!(plan.conflicts.len(), 1, "{:?}", plan.conflicts);
        assert_eq!(plan.conflicts[0].path, "a.txt");
        assert!(matches!(
            land::land(&data(&d), "chat", &d.project(), &landable(&o), false),
            Err(LandError::Conflicts(_))
        ));
        assert_eq!(project_file(&d, "a.txt").as_deref(), Some("user's edit\n"));
        // Land anyway: the chat's delete wins.
        land::land(&data(&d), "chat", &d.project(), &landable(&o), true)
            .unwrap()
            .unwrap();
        assert!(project_file(&d, "a.txt").is_none());
    }

    /// A file the chat made and deleted again leaves nothing to land: the
    /// user's own file there later is theirs, and stays.
    #[test]
    fn a_file_the_chat_made_and_deleted_doesnt_delete_the_users_file_there_later() {
        let d = setup(&[("a.txt", "a\n")]);
        let o = overlay(&d);
        let f = o
            .create(ROOT, "notes.md", Kind::File, 0o644, 0, 0, None)
            .unwrap();
        o.write(f.ino, 0, b"chat's notes\n").unwrap();
        o.unlink(ROOT, "notes.md").unwrap();
        fs::write(d.project().join("notes.md"), "user's notes\n").unwrap();
        assert!(landable(&o).is_empty(), "{:?}", landable(&o));
        land::land(&data(&d), "chat", &d.project(), &landable(&o), false).unwrap();
        assert_eq!(
            project_file(&d, "notes.md").as_deref(),
            Some("user's notes\n")
        );
    }

    /// A project file already gone from the project when the chat deleted
    /// it (a branch switched meanwhile) has no record of what it was: when
    /// the project has one there again, the delete is a conflict, not done
    /// unasked; landing anyway does it.
    #[test]
    fn a_delete_with_nothing_recorded_conflicts_with_what_the_project_has_now() {
        let d = setup(&[("a.txt", "one\n")]);
        let o = overlay(&d);
        assert_eq!(read_all(&o, "a.txt"), "one\n");
        fs::remove_file(d.project().join("a.txt")).unwrap();
        o.unlink(ROOT, "a.txt").unwrap();
        fs::write(d.project().join("a.txt"), "back on the branch\n").unwrap();
        let plan = land::plan(&d.project(), &landable(&o), false);
        assert_eq!(
            plan.conflicts
                .iter()
                .map(|c| c.path.as_str())
                .collect::<Vec<_>>(),
            vec!["a.txt"]
        );
        assert!(matches!(
            land::land(&data(&d), "chat", &d.project(), &landable(&o), false),
            Err(LandError::Conflicts(_))
        ));
        assert_eq!(
            project_file(&d, "a.txt").as_deref(),
            Some("back on the branch\n")
        );
        land::land(&data(&d), "chat", &d.project(), &landable(&o), true)
            .unwrap()
            .unwrap();
        assert!(project_file(&d, "a.txt").is_none());
    }

    /// `rm -rf node_modules && npm install` in a chat: the project ignores
    /// `node_modules/` (a folder only), so the delete and the new folder
    /// are by-products, and landing leaves the project's alone.
    #[test]
    fn a_dependency_folder_deleted_and_installed_again_isnt_landed() {
        let d = setup(&[
            (".gitignore", "node_modules/\n"),
            ("node_modules/x/index.js", "old\n"),
            ("src/a.js", "a\n"),
        ]);
        let o = overlay(&d);
        let x = ino(&o, "node_modules/x");
        o.unlink(x, "index.js").unwrap();
        o.rmdir(ino(&o, "node_modules"), "x").unwrap();
        o.rmdir(ROOT, "node_modules").unwrap();
        let deleted = o
            .changes()
            .into_iter()
            .find(|c| c.path() == "node_modules")
            .unwrap();
        assert!(deleted.by_product(), "{deleted:?}");
        let nm = o
            .create(ROOT, "node_modules", Kind::Dir, 0o755, 0, 0, None)
            .unwrap();
        let f = o
            .create(nm.ino, "y.js", Kind::File, 0o644, 0, 0, None)
            .unwrap();
        o.write(f.ino, 0, b"new\n").unwrap();
        assert!(landable(&o).is_empty(), "{:?}", landable(&o));
        land::land(&data(&d), "chat", &d.project(), &landable(&o), false).unwrap();
        assert_eq!(
            project_file(&d, "node_modules/x/index.js").as_deref(),
            Some("old\n")
        );
        assert!(project_file(&d, "node_modules/y.js").is_none());
    }

    #[test]
    fn a_delete_already_done_in_the_project_is_nothing_to_land() {
        let d = setup(&[("a.txt", "one\n")]);
        let o = overlay(&d);
        o.unlink(ROOT, "a.txt").unwrap();
        // The change list names it while the project has it…
        assert_eq!(landable(&o).len(), 1);
        fs::remove_file(d.project().join("a.txt")).unwrap();
        // …and once it's gone, there's nothing to do.
        assert!(land::plan(&d.project(), &landable(&o), false).is_empty());
    }

    #[test]
    fn a_deleted_folder_that_gained_files_in_the_project_conflicts() {
        let d = setup(&[("x/a.txt", "a\n"), ("x/b.txt", "b\n")]);
        let o = overlay(&d);
        let x = ino(&o, "x");
        o.unlink(x, "a.txt").unwrap();
        o.unlink(x, "b.txt").unwrap();
        o.rmdir(ROOT, "x").unwrap();
        fs::write(d.project().join("x/new.txt"), "user's\n").unwrap();
        let plan = land::plan(&d.project(), &landable(&o), false);
        assert_eq!(
            plan.conflicts
                .iter()
                .map(|c| c.path.as_str())
                .collect::<Vec<_>>(),
            vec!["x"]
        );
        // Unchanged, it goes.
        fs::remove_file(d.project().join("x/new.txt")).unwrap();
        land::land(&data(&d), "chat", &d.project(), &landable(&o), false)
            .unwrap()
            .unwrap();
        assert!(!d.project().join("x").exists());
    }

    #[test]
    fn a_moved_and_edited_file_merges_with_the_projects_change() {
        let d = setup(&[("a.txt", "1\n2\n3\n4\n5\n")]);
        let o = overlay(&d);
        write_all(&o, "a.txt", "ONE\n2\n3\n4\n5\n");
        o.rename(ROOT, "a.txt", ROOT, "b.txt", RenameMode::Replace)
            .unwrap();
        // The project changes another line of the original meanwhile.
        fs::write(d.project().join("a.txt"), "1\n2\n3\n4\nFIVE\n").unwrap();
        land::land(&data(&d), "chat", &d.project(), &landable(&o), false)
            .unwrap()
            .unwrap();
        assert_eq!(
            project_file(&d, "b.txt").as_deref(),
            Some("ONE\n2\n3\n4\nFIVE\n")
        );
        assert!(project_file(&d, "a.txt").is_none());
    }

    /// A moved file's new place must be free, or hold the file as it was
    /// or as the chat has it (the user moved it too): another file there
    /// is theirs, not written over.
    #[test]
    fn a_moved_file_doesnt_write_over_another_file_at_its_new_place() {
        let d = setup(&[("a.txt", "1\n")]);
        let o = overlay(&d);
        write_all(&o, "a.txt", "A\n");
        o.rename(ROOT, "a.txt", ROOT, "b.txt", RenameMode::Replace)
            .unwrap();
        fs::write(d.project().join("b.txt"), "theirs\n").unwrap();
        let plan = land::plan(&d.project(), &landable(&o), false);
        let conflicts: Vec<(&str, &str)> = plan
            .conflicts
            .iter()
            .map(|c| (c.path.as_str(), c.reason.as_str()))
            .collect();
        assert_eq!(
            conflicts,
            [("b.txt", "the project has another file there now")]
        );
        // Land anyway: the chat's.
        let anyway = land::plan(&d.project(), &landable(&o), true);
        assert!(anyway.conflicts.is_empty(), "{:?}", anyway.conflicts);
    }

    #[test]
    fn a_file_the_user_moved_too_lands_as_the_chats() {
        let d = setup(&[("a.txt", "same\n")]);
        let o = overlay(&d);
        write_all(&o, "a.txt", "edited\n");
        o.rename(ROOT, "a.txt", ROOT, "b.txt", RenameMode::Replace)
            .unwrap();
        fs::rename(d.project().join("a.txt"), d.project().join("b.txt")).unwrap();
        land::land(&data(&d), "chat", &d.project(), &landable(&o), false)
            .unwrap()
            .unwrap();
        assert_eq!(project_file(&d, "b.txt").as_deref(), Some("edited\n"));
        assert!(project_file(&d, "a.txt").is_none());
    }

    #[test]
    fn a_moved_untouched_file_lands_at_its_new_place() {
        let d = setup(&[("a.txt", "same\n")]);
        let o = overlay(&d);
        o.rename(ROOT, "a.txt", ROOT, "b.txt", RenameMode::Replace)
            .unwrap();
        land::land(&data(&d), "chat", &d.project(), &landable(&o), false)
            .unwrap()
            .unwrap();
        assert_eq!(project_file(&d, "b.txt").as_deref(), Some("same\n"));
        assert!(project_file(&d, "a.txt").is_none());
    }

    #[test]
    fn a_file_deleted_and_written_anew_merges_like_an_edit() {
        let d = setup(&[("gen.txt", "1\n2\n3\n4\n5\n")]);
        let o = overlay(&d);
        o.unlink(ROOT, "gen.txt").unwrap();
        let n = o
            .create(ROOT, "gen.txt", Kind::File, 0o644, 0, 0, None)
            .unwrap();
        o.write(n.ino, 0, b"GEN\n2\n3\n4\n5\n").unwrap();
        fs::write(d.project().join("gen.txt"), "1\n2\n3\n4\nUSER\n").unwrap();
        let plan = land::plan(&d.project(), &landable(&o), false);
        assert!(plan.conflicts.is_empty(), "{:?}", plan.conflicts);
        land::land(&data(&d), "chat", &d.project(), &landable(&o), false)
            .unwrap()
            .unwrap();
        assert_eq!(
            project_file(&d, "gen.txt").as_deref(),
            Some("GEN\n2\n3\n4\nUSER\n")
        );
    }

    #[test]
    fn a_landing_that_fails_partway_puts_everything_back() {
        let _hook = land::FAIL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = setup(&[("a.txt", "a\n"), ("b.txt", "b\n"), ("c.txt", "c\n")]);
        let o = overlay(&d);
        for f in ["a.txt", "b.txt", "c.txt"] {
            write_all(&o, f, "CHAT\n");
        }
        *land::FAIL_WRITING.lock().unwrap() = Some((d.project(), "b.txt".into()));
        let result = land::land(&data(&d), "chat-fail", &d.project(), &landable(&o), false);
        *land::FAIL_WRITING.lock().unwrap() = None;
        assert!(
            matches!(result, Err(LandError::Failed(ref e)) if e.contains("nothing landed")),
            "{result:?}"
        );
        for (f, was) in [("a.txt", "a\n"), ("b.txt", "b\n"), ("c.txt", "c\n")] {
            assert_eq!(project_file(&d, f).as_deref(), Some(was), "{f}");
        }
        assert!(
            land::latest(&data(&d), "chat-fail").is_none(),
            "nothing to undo"
        );
        // Nothing for recovery to finish either.
        assert!(land::recover(&data(&d)).is_empty());
    }

    /// P1.3: a landing that fails and can't put everything back stays
    /// "applying": it's interrupted until recovery finishes it.
    #[test]
    fn a_landing_that_cant_be_put_back_is_finished_by_recovery() {
        let _hook = land::FAIL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = setup(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
        let o = overlay(&d);
        for f in ["a.txt", "b.txt"] {
            write_all(&o, f, "CHAT\n");
        }
        // a.txt fails, and so does putting it back.
        *land::FAIL_WRITING.lock().unwrap() = Some((d.project(), "a.txt".into()));
        land::FAIL_WRITING_ALWAYS.store(true, std::sync::atomic::Ordering::SeqCst);
        let result = land::land(&data(&d), "chat-half", &d.project(), &landable(&o), false);
        land::FAIL_WRITING_ALWAYS.store(false, std::sync::atomic::Ordering::SeqCst);
        *land::FAIL_WRITING.lock().unwrap() = None;
        assert!(
            matches!(result, Err(LandError::Failed(ref e)) if e.contains("couldn't be put back")),
            "{result:?}"
        );
        assert!(land::interrupted_into(&data(&d), "", &d.project()));
        land::recover_into(&data(&d), "", &d.project());
        assert!(!land::interrupted_into(&data(&d), "", &d.project()));
        for f in ["a.txt", "b.txt"] {
            assert_eq!(project_file(&d, f).as_deref(), Some("CHAT\n"), "{f}");
        }
    }

    /// P6.9: a child's landing into its parent's view is found by the
    /// parent's id, never finished at start as if into a project, and
    /// finished into the view wherever it's mounted now.
    #[test]
    fn a_childs_landing_is_found_by_its_parent_and_finished_where_the_view_is() {
        let _hook = land::FAIL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = setup(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
        let o = overlay(&d);
        for f in ["a.txt", "b.txt"] {
            write_all(&o, f, "CHILD\n");
        }
        land::mark_into_chat(&data(&d), "kid", "parent-p").unwrap();
        *land::FAIL_WRITING.lock().unwrap() = Some((d.project(), "a.txt".into()));
        land::FAIL_WRITING_ALWAYS.store(true, std::sync::atomic::Ordering::SeqCst);
        let result = land::land(&data(&d), "kid", &d.project(), &landable(&o), false);
        land::FAIL_WRITING_ALWAYS.store(false, std::sync::atomic::Ordering::SeqCst);
        *land::FAIL_WRITING.lock().unwrap() = None;
        assert!(result.is_err());
        // The parent's view is mounted somewhere else now.
        let moved = d.root.join("view-now");
        fs::rename(d.project(), &moved).unwrap();
        assert!(land::interrupted_into(&data(&d), "parent-p", &moved));
        assert!(!land::interrupted_into(&data(&d), "another", &d.project()));
        assert!(land::recover(&data(&d)).is_empty(), "not at start");
        land::recover_into(&data(&d), "parent-p", &moved);
        assert!(!land::interrupted_into(&data(&d), "parent-p", &moved));
        for f in ["a.txt", "b.txt"] {
            assert_eq!(
                fs::read_to_string(moved.join(f)).ok().as_deref(),
                Some("CHILD\n"),
                "{f}"
            );
        }
    }

    #[test]
    fn recovery_finishes_only_paths_nobody_changed_since() {
        let d = setup(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
        let o = overlay(&d);
        write_all(&o, "a.txt", "A\n");
        write_all(&o, "b.txt", "B\n");
        let landed = land::land(&data(&d), "chat", &d.project(), &landable(&o), false)
            .unwrap()
            .unwrap();
        // As if the app crashed before writing anything: the project as it
        // was, the journal "applying"; then the user edits b.txt.
        let journal = data(&d)
            .join("landings")
            .join("chat")
            .join(&landed.id)
            .join("journal.json");
        let text = fs::read_to_string(&journal)
            .unwrap()
            .replace("\"done\"", "\"applying\"");
        fs::write(&journal, text).unwrap();
        fs::write(d.project().join("a.txt"), "a\n").unwrap();
        fs::write(d.project().join("b.txt"), "user's\n").unwrap();
        let report = land::recover(&data(&d));
        assert_eq!(project_file(&d, "a.txt").as_deref(), Some("A\n"));
        assert_eq!(project_file(&d, "b.txt").as_deref(), Some("user's\n"));
        assert!(
            report.iter().any(|r| r.starts_with("b.txt (not finished")),
            "{report:?}"
        );
    }

    /// A folder the chat deleted, journaled whole; the app crashes before
    /// the landing writes, and the user fills the folder with other files:
    /// recovery sees it isn't the journal's copy, and leaves it.
    #[test]
    fn recovery_leaves_a_folder_the_user_filled_again() {
        let d = setup(&[("dir/f.txt", "f\n"), ("b.txt", "b\n")]);
        let o = overlay(&d);
        let dir = ino(&o, "dir");
        o.unlink(dir, "f.txt").unwrap();
        o.rmdir(ROOT, "dir").unwrap();
        write_all(&o, "b.txt", "B\n");
        let landed = land::land(&data(&d), "chat", &d.project(), &landable(&o), false)
            .unwrap()
            .unwrap();
        let journal = data(&d)
            .join("landings")
            .join("chat")
            .join(&landed.id)
            .join("journal.json");
        let text = fs::read_to_string(&journal)
            .unwrap()
            .replace("\"done\"", "\"applying\"");
        fs::write(&journal, text).unwrap();
        fs::write(d.project().join("b.txt"), "b\n").unwrap();
        fs::create_dir_all(d.project().join("dir")).unwrap();
        fs::write(d.project().join("dir/new.txt"), "the user's\n").unwrap();
        let report = land::recover(&data(&d));
        assert_eq!(project_file(&d, "b.txt").as_deref(), Some("B\n"));
        assert_eq!(
            project_file(&d, "dir/new.txt").as_deref(),
            Some("the user's\n"),
            "{report:?}"
        );
        assert!(
            report.iter().any(|r| r.starts_with("dir (not finished")),
            "{report:?}"
        );
    }

    #[test]
    fn undo_that_stopped_partway_finishes_on_retry() {
        let d = setup(&[("a.txt", "one\n"), ("b.txt", "two\n")]);
        let o = overlay(&d);
        write_all(&o, "a.txt", "ONE\n");
        write_all(&o, "b.txt", "TWO\n");
        let landed = land::land(&data(&d), "chat", &d.project(), &landable(&o), false)
            .unwrap()
            .unwrap();
        // As if the first undo put a.txt back, then failed before b.txt.
        fs::write(d.project().join("a.txt"), "one\n").unwrap();
        let kept = land::undo(&data(&d), "chat", &d.project(), &landed.id).unwrap();
        assert!(kept.is_empty(), "{kept:?}");
        assert_eq!(project_file(&d, "a.txt").as_deref(), Some("one\n"));
        assert_eq!(project_file(&d, "b.txt").as_deref(), Some("two\n"));
    }

    #[test]
    fn undo_keeps_files_added_to_a_landed_folder() {
        let d = setup(&[]);
        let o = overlay(&d);
        let dir = o
            .create(ROOT, "feat", Kind::Dir, 0o755, 0, 0, None)
            .unwrap();
        let f = o
            .create(dir.ino, "a.txt", Kind::File, 0o644, 0, 0, None)
            .unwrap();
        o.write(f.ino, 0, b"a\n").unwrap();
        let landed = land::land(&data(&d), "chat", &d.project(), &landable(&o), false)
            .unwrap()
            .unwrap();
        fs::write(d.project().join("feat/b.txt"), "user's\n").unwrap();
        let kept = land::undo(&data(&d), "chat", &d.project(), &landed.id).unwrap();
        assert!(
            project_file(&d, "feat/a.txt").is_none(),
            "the landed file goes"
        );
        assert_eq!(
            project_file(&d, "feat/b.txt").as_deref(),
            Some("user's\n"),
            "the user's stays"
        );
        assert_eq!(
            kept.iter().map(|k| k.path.as_str()).collect::<Vec<_>>(),
            vec!["feat"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn undoing_a_deleted_folder_brings_back_its_links() {
        let d = setup(&[("x/a.txt", "a\n")]);
        std::os::unix::fs::symlink("a.txt", d.project().join("x/link")).unwrap();
        let o = overlay(&d);
        let x = ino(&o, "x");
        o.unlink(x, "a.txt").unwrap();
        o.unlink(x, "link").unwrap();
        o.rmdir(ROOT, "x").unwrap();
        let landed = land::land(&data(&d), "chat", &d.project(), &landable(&o), false)
            .unwrap()
            .unwrap();
        assert!(!d.project().join("x").exists());
        land::undo(&data(&d), "chat", &d.project(), &landed.id).unwrap();
        assert_eq!(
            fs::read_link(d.project().join("x/link")).unwrap(),
            PathBuf::from("a.txt")
        );
    }

    #[test]
    fn a_crash_after_a_replace_keeps_the_chats_file_readable() {
        let d = setup(&[("a.txt", "one\n")]);
        {
            let o = overlay(&d);
            write_all(&o, "a.txt", "first\n");
            o.flush();
            // `sed -i`: a new file renamed over it, and no save before the crash.
            let t = o
                .create(ROOT, "tmp", Kind::File, 0o644, 0, 0, None)
                .unwrap();
            o.write(t.ino, 0, b"second\n").unwrap();
            o.rename(ROOT, "tmp", ROOT, "a.txt", RenameMode::Replace)
                .unwrap();
        }
        // The saved list still names the first version, whose file is still there.
        let o = overlay(&d);
        assert_eq!(read_all(&o, "a.txt"), "first\n");
        // Once saved, files nothing names are cleaned up when the chat's own
        // overlay opens (chats::open sweeps).
        o.flush();
        drop(o);
        overlay(&d).sweep_orphans();
        let files = fs::read_dir(d.store().join("upper")).unwrap().count();
        assert_eq!(files, 1, "only the file the list names stays");
    }

    #[test]
    fn a_hard_link_survives_its_original_leaving_the_project() {
        let d = setup(&[("a.txt", "linked\n")]);
        let o = overlay(&d);
        let a = ino(&o, "a.txt");
        o.link(a, ROOT, "keep.txt").unwrap();
        fs::remove_file(d.project().join("a.txt")).unwrap();
        // The folder is listed again (as after the project changed).
        let _ = o.readdir_plus(ROOT);
        assert_eq!(read_all(&o, "keep.txt"), "linked\n");
    }

    #[test]
    fn rebasing_swaps_content_and_base_together() {
        let d = setup(&[("a.txt", "1\n2\n3\n")]);
        let o = overlay(&d);
        write_all(&o, "a.txt", "A\n2\n3\n");
        let (_, old_base) = o.versions("a.txt").unwrap();
        o.rebase_file("a.txt", Some(b"merged\n"), Some(b"project\n"))
            .unwrap();
        assert_eq!(read_all(&o, "a.txt"), "merged\n");
        let (upper, base) = o.versions("a.txt").unwrap();
        assert_eq!(fs::read(base).unwrap(), b"project\n");
        assert_eq!(fs::read(upper).unwrap(), b"merged\n");
        // The old base goes once the list is saved (rebase_file saves).
        assert!(!old_base.exists());
    }
}

/// Fixes from the second review.
mod second_review {
    use super::*;
    use crate::overlay::land;

    fn landable(o: &OverlayFs) -> Vec<OverlayChange> {
        o.changes()
            .into_iter()
            .filter(|c| !c.by_product())
            .collect()
    }

    #[test]
    fn a_delete_conflict_is_settled_by_either_side() {
        // Keeping the delete.
        let d = setup(&[("a.txt", "one\n")]);
        let o = overlay(&d);
        o.unlink(ROOT, "a.txt").unwrap();
        fs::write(d.project().join("a.txt"), "user\n").unwrap();
        assert_eq!(
            land::plan(&d.project(), &landable(&o), false)
                .conflicts
                .len(),
            1
        );
        o.rebase_file("a.txt", None, Some(b"user\n")).unwrap();
        assert!(land::plan(&d.project(), &landable(&o), false)
            .conflicts
            .is_empty());
        // Keeping the project's file.
        let d = setup(&[("a.txt", "one\n")]);
        let o = overlay(&d);
        o.unlink(ROOT, "a.txt").unwrap();
        fs::write(d.project().join("a.txt"), "user\n").unwrap();
        o.rebase_file("a.txt", Some(b"user\n"), Some(b"user\n"))
            .unwrap();
        let plan = land::plan(&d.project(), &landable(&o), false);
        assert!(plan.conflicts.is_empty(), "{:?}", plan.conflicts);
        assert!(plan.is_empty(), "the project already has it");
    }

    #[test]
    fn a_folder_deleted_whole_still_guards_the_files_the_chat_changed_in_it() {
        let d = setup(&[("x/a.txt", "1\n"), ("x/b.txt", "b\n")]);
        let o = overlay(&d);
        write_all(&o, "x/a.txt", "chat\n");
        // Another chat lands a change to x/a.txt, then this one runs rm -rf x.
        fs::write(d.project().join("x/a.txt"), "other chat\n").unwrap();
        let x = ino(&o, "x");
        o.unlink(x, "a.txt").unwrap();
        o.unlink(x, "b.txt").unwrap();
        o.rmdir(ROOT, "x").unwrap();
        let plan = land::plan(&d.project(), &landable(&o), false);
        assert_eq!(
            plan.conflicts
                .iter()
                .map(|c| c.path.as_str())
                .collect::<Vec<_>>(),
            vec!["x"]
        );
    }

    #[test]
    fn undoing_a_file_turned_folder_keeps_what_was_added_to_it() {
        let d = setup(&[("x", "a file\n")]);
        let o = overlay(&d);
        o.unlink(ROOT, "x").unwrap();
        let dir = o.create(ROOT, "x", Kind::Dir, 0o755, 0, 0, None).unwrap();
        let f = o
            .create(dir.ino, "a.txt", Kind::File, 0o644, 0, 0, None)
            .unwrap();
        o.write(f.ino, 0, b"a\n").unwrap();
        let data = d.root.join("data");
        let landed = land::land(&data, "chat", &d.project(), &landable(&o), false)
            .unwrap()
            .unwrap();
        fs::write(d.project().join("x/b.txt"), "user's\n").unwrap();
        let kept = land::undo(&data, "chat", &d.project(), &landed.id).unwrap();
        assert_eq!(
            fs::read_to_string(d.project().join("x/b.txt")).unwrap(),
            "user's\n"
        );
        assert!(kept.iter().any(|k| k.path == "x"), "{kept:?}");
    }

    #[test]
    fn a_file_moved_away_and_back_isnt_remembered_as_deleted() {
        let d = setup(&[("a.txt", "one\n")]);
        let o = overlay(&d);
        o.rename(ROOT, "a.txt", ROOT, "b.txt", RenameMode::Replace)
            .unwrap();
        o.rename(ROOT, "b.txt", ROOT, "a.txt", RenameMode::Replace)
            .unwrap();
        fs::write(d.project().join("a.txt"), "changed\n").unwrap();
        // The chat sees the new content, then deletes it: that's what it deleted.
        o.unlink(ROOT, "a.txt").unwrap();
        assert!(land::plan(&d.project(), &landable(&o), false)
            .conflicts
            .is_empty());
    }
}

/// A workspace chat's landing (workspaces-plan.md, 4.6, D24): its changes in
/// two folders, `api` and `web`, each its own overlay over its own project,
/// land under one journal, all or nothing.
mod across_folders {
    use super::*;
    use crate::folders::{Folder, Folders};
    use crate::overlay::land::{self, LandError};
    use std::path::Path;

    /// Where the landings are kept: beside api's project.
    fn data(api: &Dirs) -> PathBuf {
        api.root.join("data")
    }

    fn folders(api: &Path, web: &Path) -> Folders {
        Folders::named(vec![
            Folder {
                name: "api".into(),
                path: api.to_path_buf(),
            },
            Folder {
                name: "web".into(),
                path: web.to_path_buf(),
            },
        ])
        .unwrap()
    }

    /// One folder's changes as the chat's view names them: its name first.
    fn named(name: &str, changes: Vec<OverlayChange>) -> Vec<OverlayChange> {
        changes
            .into_iter()
            .filter(|c| !c.by_product())
            .map(|c| c.under(name))
            .collect()
    }

    /// The chat's changes across both folders.
    fn both(api: &OverlayFs, web: &OverlayFs) -> Vec<OverlayChange> {
        let mut all = named("api", api.changes());
        all.extend(named("web", web.changes()));
        all
    }

    fn read(project: &Path, rel: &str) -> Option<String> {
        fs::read_to_string(project.join(rel)).ok()
    }

    /// api with `a.txt`, web with `b.txt`, each changed by the chat.
    fn changed() -> (Dirs, Dirs, OverlayFs, OverlayFs) {
        let (api_dirs, web_dirs) = (setup(&[("a.txt", "one\n")]), setup(&[("b.txt", "b\n")]));
        let (api, web) = (overlay(&api_dirs), overlay(&web_dirs));
        write_all(&api, "a.txt", "ONE\n");
        write_all(&web, "b.txt", "B\n");
        (api_dirs, web_dirs, api, web)
    }

    fn journal(data: &Path, chat: &str, id: &str) -> serde_json::Value {
        let file = data
            .join("landings")
            .join(chat)
            .join(id)
            .join("journal.json");
        serde_json::from_slice(&fs::read(file).unwrap()).unwrap()
    }

    /// As if the app died after writing the journal, before the files: each
    /// folder's file back as it was, the journal still applying.
    fn interrupt(data: &Path, chat: &str, id: &str) {
        let file = data
            .join("landings")
            .join(chat)
            .join(id)
            .join("journal.json");
        let text = fs::read_to_string(&file)
            .unwrap()
            .replace("\"done\"", "\"applying\"");
        fs::write(&file, text).unwrap();
    }

    #[test]
    fn changes_in_two_folders_land_under_one_landing() {
        let (a, w, api, web) = changed();
        let n = web
            .create(ROOT, "n.txt", Kind::File, 0o644, 0, 0, None)
            .unwrap();
        web.write(n.ino, 0, b"N\n").unwrap();
        let ws = folders(&a.project(), &w.project());
        let landed = land::land_in(&data(&a), "ws", &ws, &both(&api, &web), false)
            .unwrap()
            .unwrap();
        let mut paths = landed.paths.clone();
        paths.sort();
        assert_eq!(paths, ["api/a.txt", "web/b.txt", "web/n.txt"]);
        assert_eq!(read(&a.project(), "a.txt").as_deref(), Some("ONE\n"));
        assert_eq!(read(&w.project(), "b.txt").as_deref(), Some("B\n"));
        assert_eq!(read(&w.project(), "n.txt").as_deref(), Some("N\n"));
        let landings = data(&a).join("landings").join("ws");
        let ids: Vec<_> = fs::read_dir(&landings)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir())
            .collect();
        assert_eq!(ids.len(), 1, "one landing");
        // Its folders are kept for recovery and undo; no project.txt.
        assert!(landings.join("folders.json").exists());
        assert!(!landings.join("project.txt").exists());
        assert_eq!(
            journal(&data(&a), "ws", &landed.id)["folders"],
            serde_json::json!(["api", "web"])
        );

        let kept = land::undo_in(&data(&a), "ws", &ws, &landed.id).unwrap();
        assert!(kept.is_empty(), "{kept:?}");
        assert_eq!(read(&a.project(), "a.txt").as_deref(), Some("one\n"));
        assert_eq!(read(&w.project(), "b.txt").as_deref(), Some("b\n"));
        assert_eq!(read(&w.project(), "n.txt"), None);
    }

    #[test]
    fn a_conflict_in_one_folder_lands_nothing_in_any() {
        let (a, w, api, web) = changed();
        fs::write(w.project().join("b.txt"), "theirs\n").unwrap();
        let ws = folders(&a.project(), &w.project());
        let Err(LandError::Conflicts(conflicts)) =
            land::land_in(&data(&a), "ws", &ws, &both(&api, &web), false)
        else {
            panic!("a conflict stops it");
        };
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].path, "web/b.txt", "named under its folder");
        assert_eq!(read(&a.project(), "a.txt").as_deref(), Some("one\n"));
    }

    #[test]
    fn a_move_within_a_folder_lands_as_a_move() {
        let (a, w) = (setup(&[("old.txt", "x\n")]), setup(&[]));
        let (api, web) = (overlay(&a), overlay(&w));
        api.rename(ROOT, "old.txt", ROOT, "new.txt", RenameMode::Replace)
            .unwrap();
        let ws = folders(&a.project(), &w.project());
        land::land_in(&data(&a), "ws", &ws, &both(&api, &web), false)
            .unwrap()
            .unwrap();
        assert_eq!(read(&a.project(), "new.txt").as_deref(), Some("x\n"));
        assert_eq!(read(&a.project(), "old.txt"), None);
    }

    #[test]
    fn a_failed_write_in_the_second_folder_puts_the_first_back() {
        let _hook = land::FAIL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (a, w, api, web) = changed();
        let ws = folders(&a.project(), &w.project());
        *land::FAIL_WRITING.lock().unwrap() = Some((w.project(), "b.txt".into()));
        let failed = land::land_in(&data(&a), "ws", &ws, &both(&api, &web), false);
        *land::FAIL_WRITING.lock().unwrap() = None;
        let Err(LandError::Failed(why)) = failed else {
            panic!("the write fails: {failed:?}");
        };
        assert!(why.contains("nothing landed"), "{why}");
        assert_eq!(read(&a.project(), "a.txt").as_deref(), Some("one\n"));
        assert_eq!(read(&w.project(), "b.txt").as_deref(), Some("b\n"));
    }

    #[test]
    fn an_interrupted_two_folder_landing_is_finished_at_start() {
        let (a, w, api, web) = changed();
        let ws = folders(&a.project(), &w.project());
        let landed = land::land_in(&data(&a), "ws", &ws, &both(&api, &web), false)
            .unwrap()
            .unwrap();
        fs::write(a.project().join("a.txt"), "one\n").unwrap();
        fs::write(w.project().join("b.txt"), "b\n").unwrap();
        interrupt(&data(&a), "ws", &landed.id);
        let mut done = land::recover(&data(&a));
        done.sort();
        assert_eq!(done, ["api/a.txt", "web/b.txt"]);
        assert_eq!(read(&a.project(), "a.txt").as_deref(), Some("ONE\n"));
        assert_eq!(read(&w.project(), "b.txt").as_deref(), Some("B\n"));
    }

    #[test]
    fn a_landing_whose_folder_is_missing_at_start_waits_for_it_where_its_found() {
        let (a, w, api, web) = changed();
        let ws = folders(&a.project(), &w.project());
        let landed = land::land_in(&data(&a), "ws", &ws, &both(&api, &web), false)
            .unwrap()
            .unwrap();
        fs::write(a.project().join("a.txt"), "one\n").unwrap();
        fs::write(w.project().join("b.txt"), "b\n").unwrap();
        interrupt(&data(&a), "ws", &landed.id);
        // web moved away while the app was closed.
        let moved = w.root.join("moved");
        fs::rename(w.project(), &moved).unwrap();
        let done = land::recover(&data(&a));
        assert!(done.contains(&"api/a.txt".to_string()), "{done:?}");
        assert!(
            done.contains(&"web/b.txt (not finished: its folder isn't there)".to_string()),
            "{done:?}"
        );
        assert!(!w.project().exists(), "web isn't made anew");
        assert_eq!(read(&a.project(), "a.txt").as_deref(), Some("ONE\n"));
        assert_eq!(
            journal(&data(&a), "ws", &landed.id)["state"],
            "applying",
            "web's part waits"
        );

        // Located where it went: finished there.
        land::relocate(&data(&a), "ws", "web", &moved).unwrap();
        assert_eq!(land::recover(&data(&a)), ["web/b.txt"]);
        assert_eq!(read(&moved, "b.txt").as_deref(), Some("B\n"));
        assert_eq!(journal(&data(&a), "ws", &landed.id)["state"], "done");
    }

    #[test]
    fn undo_puts_back_every_folder_also_once_the_chats_store_is_gone() {
        let (a, w, api, web) = changed();
        let ws = folders(&a.project(), &w.project());
        let landed = land::land_in(&data(&a), "ws", &ws, &both(&api, &web), false)
            .unwrap()
            .unwrap();
        drop((api, web));
        fs::remove_dir_all(a.store()).unwrap();
        fs::remove_dir_all(w.store()).unwrap();
        let kept_folders = land::folders_of(&data(&a), "ws").unwrap();
        assert_eq!(kept_folders, ws);
        let kept = land::undo_in(&data(&a), "ws", &kept_folders, &landed.id).unwrap();
        assert!(kept.is_empty(), "{kept:?}");
        assert_eq!(read(&a.project(), "a.txt").as_deref(), Some("one\n"));
        assert_eq!(read(&w.project(), "b.txt").as_deref(), Some("b\n"));
    }

    #[test]
    fn land_and_undo_are_refused_while_a_folder_is_missing() {
        let (a, w, api, web) = changed();
        let ws = folders(&a.project(), &w.project());
        let changes = both(&api, &web);
        let landed = land::land_in(&data(&a), "ws", &ws, &changes, false)
            .unwrap()
            .unwrap();
        let moved = w.root.join("moved");
        fs::rename(w.project(), &moved).unwrap();
        let refused = land::undo_in(&data(&a), "ws", &ws, &landed.id).unwrap_err();
        assert!(refused.starts_with("web isn't there"), "{refused}");
        let Err(LandError::Failed(refused)) = land::land_in(&data(&a), "ws", &ws, &changes, false)
        else {
            panic!("refused");
        };
        assert!(refused.starts_with("web isn't there"), "{refused}");
        assert!(!w.project().exists(), "nothing made in its place");
        assert_eq!(read(&a.project(), "a.txt").as_deref(), Some("ONE\n"));
    }

    /// Landings that share a folder wait for each other, whatever order
    /// their chats list their folders in, and neither waits forever.
    #[test]
    fn two_chats_landing_at_once_both_finish() {
        let (a, w) = (
            setup(&[("a1.txt", "1\n"), ("a2.txt", "2\n")]),
            setup(&[("b1.txt", "1\n"), ("b2.txt", "2\n")]),
        );
        let (one_api, one_web) = (overlay(&a), overlay(&w));
        write_all(&one_api, "a1.txt", "ONE\n");
        write_all(&one_web, "b1.txt", "ONE\n");
        let first = both(&one_api, &one_web);
        let second_store = a.root.join("second");
        let alone = OverlayFs::open(&a.project(), &second_store, 1 << 30).unwrap();
        write_all(&alone, "a2.txt", "TWO\n");
        let (other_api, other_web) = (
            OverlayFs::open(&a.project(), &a.root.join("third-api"), 1 << 30).unwrap(),
            OverlayFs::open(&w.project(), &w.root.join("third-web"), 1 << 30).unwrap(),
        );
        write_all(&other_web, "b2.txt", "TWO\n");
        let third = both(&other_api, &other_web);
        let in_order = folders(&a.project(), &w.project());
        // The same folders, the other way round.
        let reversed = Folders::named(in_order.iter().rev().cloned().collect()).unwrap();
        let data = data(&a);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|s| {
            let jobs: Vec<Box<dyn FnOnce() -> bool + Send + '_>> = vec![
                Box::new(|| land::land_in(&data, "first", &in_order, &first, false).is_ok()),
                Box::new(|| {
                    let changes = alone.changes();
                    land::land(&data, "alone", &a.project(), &changes, false).is_ok()
                }),
                Box::new(|| land::land_in(&data, "third", &reversed, &third, false).is_ok()),
            ];
            for job in jobs {
                let tx = tx.clone();
                s.spawn(move || tx.send(job()).unwrap());
            }
            for _ in 0..3 {
                let finished = rx
                    .recv_timeout(std::time::Duration::from_secs(20))
                    .expect("a landing waited forever");
                assert!(finished);
            }
        });
        assert_eq!(read(&a.project(), "a1.txt").as_deref(), Some("ONE\n"));
        assert_eq!(read(&a.project(), "a2.txt").as_deref(), Some("TWO\n"));
        assert_eq!(read(&w.project(), "b1.txt").as_deref(), Some("ONE\n"));
        assert_eq!(read(&w.project(), "b2.txt").as_deref(), Some("TWO\n"));
    }

    #[test]
    fn a_folder_chats_journal_has_no_folders() {
        let d = setup(&[("a.txt", "one\n")]);
        let o = overlay(&d);
        write_all(&o, "a.txt", "ONE\n");
        let data = d.root.join("data");
        let landed = land::land(&data, "chat", &d.project(), &o.changes(), false)
            .unwrap()
            .unwrap();
        assert!(journal(&data, "chat", &landed.id).get("folders").is_none());
        let landings = data.join("landings").join("chat");
        assert!(landings.join("project.txt").exists());
        assert!(!landings.join("folders.json").exists());
        assert_eq!(
            land::folders_of(&data, "chat").unwrap().single(),
            Some(d.project().as_path())
        );
    }
}

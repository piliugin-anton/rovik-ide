//! Snapshots of fixture projects (T01, 6.4): what goes in and what stays
//! out, modes, links and line endings, the stat cache, the parent and its
//! fallbacks, the guards and packing, and a project left as it was. And a
//! chat's overlay folded into the snapshot that moves it (T09).
//!
//! A test binary of its own: libgit2's search paths are the process's, and
//! these tests set them so that the user's global excludes are a fixture's
//! and the machine's git settings don't count.

use git2::{
    ConfigLevel, IndexAddOption, IndexEntry, IndexTime, ObjectType, Oid, Repository, Signature,
    TreeWalkMode, TreeWalkResult,
};
use rovik_client::shadow::{Composite, Shadow, Snapshot, LARGE_FILE};
use rovik_core::overlay::fs::{OverlayChange, OverlayFs};
use rovik_core::testing::{temp_dir, TempDir};
use rovik_core::vfs::backend::Backend;
use rovik_core::vfs::store::{Kind, RenameMode, SetAttr, ROOT};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

const CHAT: &str = "chat-1";

/// A test's folder: the project in `project/`, its shadow beside it.
struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Fixture {
        user_settings();
        Fixture { dir: temp_dir() }
    }

    fn project(&self) -> PathBuf {
        self.dir.0.join("project")
    }

    fn shadow_dir(&self) -> PathBuf {
        self.dir.0.join("shadow.git")
    }

    fn shadow(&self) -> Shadow {
        Shadow::open(&self.shadow_dir(), &self.project()).unwrap()
    }
}

/// The user's git settings for every test: global excludes that leave out
/// `*.mine`, and nothing from the machine running the tests.
fn user_settings() {
    static USER: OnceLock<TempDir> = OnceLock::new();
    USER.get_or_init(|| {
        let user = temp_dir();
        let excludes = user.0.join("ignore");
        fs::write(&excludes, "*.mine\n").unwrap();
        let config = format!("[core]\n\texcludesFile = {}\n", excludes.display());
        fs::write(user.0.join(".gitconfig"), config.replace('\\', "/")).unwrap();
        let nowhere = user.0.join("nowhere");
        // Every test comes here before it touches git, and waits until the
        // paths are set.
        unsafe {
            for level in [
                ConfigLevel::ProgramData,
                ConfigLevel::System,
                ConfigLevel::XDG,
            ] {
                git2::opts::set_search_path(level, nowhere.as_path()).unwrap();
            }
            git2::opts::set_search_path(ConfigLevel::Global, user.0.as_path()).unwrap();
        }
        user
    });
}

fn write(path: &Path, content: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

#[cfg(unix)]
fn executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn open_or_init(root: &Path) -> Repository {
    Repository::open(root).unwrap_or_else(|_| Repository::init(root).unwrap())
}

/// Commits every file under `root`, whatever the ignore rules say, to its
/// repository, created the first time.
fn commit_all(root: &Path) -> Oid {
    let repo = open_or_init(root);
    let mut index = repo.index().unwrap();
    index.add_all(["*"], IndexAddOption::FORCE, None).unwrap();
    commit_index(&repo, &mut index)
}

/// Commits `paths` alone, whatever the ignore rules say.
fn commit(root: &Path, paths: &[&str]) -> Oid {
    let repo = open_or_init(root);
    let mut index = repo.index().unwrap();
    for path in paths {
        index.add_path(Path::new(path)).unwrap();
    }
    commit_index(&repo, &mut index)
}

fn commit_index(repo: &Repository, index: &mut git2::Index) -> Oid {
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let me = Signature::now("Test", "test@example.com").unwrap();
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    let parents: Vec<_> = parent.iter().collect();
    repo.commit(Some("HEAD"), &me, &me, "work", &tree, &parents)
        .unwrap()
}

/// The snapshot's files and links by path, with their modes and contents.
fn files(shadow: &Shadow, snapshot: &Snapshot) -> BTreeMap<String, (i32, Vec<u8>)> {
    let repo = shadow.repo();
    let tree = repo.find_tree(snapshot.tree).unwrap();
    let mut files = BTreeMap::new();
    tree.walk(TreeWalkMode::PreOrder, |dir, entry| {
        if entry.kind() == Some(ObjectType::Blob) {
            let blob = repo.find_blob(entry.id()).unwrap();
            let name = String::from_utf8_lossy(entry.name_bytes());
            let file = (entry.filemode(), blob.content().to_vec());
            files.insert(format!("{dir}{name}"), file);
        }
        TreeWalkResult::Ok
    })
    .unwrap();
    files
}

fn paths(shadow: &Shadow, snapshot: &Snapshot) -> Vec<String> {
    files(shadow, snapshot).into_keys().collect()
}

fn content(shadow: &Shadow, snapshot: &Snapshot, path: &str) -> Vec<u8> {
    files(shadow, snapshot).remove(path).unwrap().1
}

fn parent(shadow: &Shadow, snapshot: &Snapshot) -> Option<Oid> {
    let commit = shadow.repo().find_commit(snapshot.commit).unwrap();
    commit.parent_id(0).ok()
}

type State = BTreeMap<PathBuf, (Vec<u8>, SystemTime)>;

/// Everything under `root`: each file's bytes, or a link's target, and each
/// entry's modification time.
fn state(root: &Path) -> State {
    let mut state = State::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).unwrap().flatten() {
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let bytes = if meta.file_type().is_symlink() {
                fs::read_link(&path)
                    .unwrap()
                    .into_os_string()
                    .into_encoded_bytes()
            } else if meta.is_dir() {
                folders.push(path.clone());
                Vec::new()
            } else {
                fs::read(&path).unwrap()
            };
            state.insert(path, (bytes, meta.modified().unwrap()));
        }
    }
    state
}

fn loose_objects(objects: &Path) -> usize {
    let fanouts = fs::read_dir(objects).unwrap().flatten();
    let fanouts = fanouts.filter(|entry| entry.file_name().len() == 2);
    fanouts
        .map(|fanout| fs::read_dir(fanout.path()).unwrap().count())
        .sum()
}

#[test]
fn ignored_files_stay_out_and_tracked_ones_stay_in() {
    let fixture = Fixture::new();
    let root = fixture.project();
    write(&root.join(".gitignore"), "*.log\n");
    write(&root.join("src/a.rs"), "fn a() {}\n");
    write(
        &root.join("keep.log"),
        "tracked, though .gitignore matches it\n",
    );
    let head = commit_all(&root);
    write(&root.join("new.txt"), "untracked\n");
    write(&root.join("debug.log"), "left out by .gitignore\n");
    write(&root.join(".git/info/exclude"), "secret.txt\n");
    write(&root.join("secret.txt"), "left out by info/exclude\n");
    write(
        &root.join("notes.mine"),
        "left out by the global excludes\n",
    );

    let shadow = fixture.shadow();
    let snapshot = shadow.snapshot(CHAT).unwrap();

    let expected = [".gitignore", "keep.log", "new.txt", "src/a.rs"];
    assert_eq!(paths(&shadow, &snapshot), expected);
    assert_eq!(parent(&shadow, &snapshot), Some(head));
    assert!(snapshot.changed);
    assert_eq!(shadow.subpath(), "");
}

#[cfg(unix)]
#[test]
fn modes_and_links_are_kept() {
    let fixture = Fixture::new();
    let root = fixture.project();
    write(&root.join("plain.txt"), "text\n");
    write(&root.join("tracked.sh"), "#!/bin/sh\n");
    executable(&root.join("tracked.sh"));
    commit_all(&root);
    write(&root.join("new.sh"), "#!/bin/sh\n");
    executable(&root.join("new.sh"));
    std::os::unix::fs::symlink("plain.txt", root.join("link")).unwrap();

    let shadow = fixture.shadow();
    let files = files(&shadow, &shadow.snapshot(CHAT).unwrap());

    assert_eq!(files["plain.txt"].0, 0o100644);
    assert_eq!(files["tracked.sh"].0, 0o100755);
    assert_eq!(files["new.sh"].0, 0o100755);
    assert_eq!(files["link"], (0o120000, b"plain.txt".to_vec()));
}

#[test]
fn after_one_edit_a_snapshot_hashes_one_file() {
    let fixture = Fixture::new();
    let root = fixture.project();
    for n in 0..20 {
        write(
            &root.join(format!("src/file{n}.rs")),
            format!("fn f{n}() {{}}\n"),
        );
    }
    commit_all(&root);
    write(&root.join("new.txt"), "untracked\n");
    let shadow = fixture.shadow();

    let first = shadow.snapshot(CHAT).unwrap();
    assert_eq!(first.hashed, 1, "only the untracked file isn't HEAD's");
    // Every entry holds its file's stat data now, so that the next snapshot
    // reads only what changed.
    for entry in shadow.repo().index().unwrap().iter() {
        let path = root.join(String::from_utf8(entry.path).unwrap());
        assert_eq!(entry.file_size as u64, fs::metadata(&path).unwrap().len());
        assert_ne!(entry.mtime.seconds(), 0, "{}", path.display());
    }

    write(&root.join("src/file7.rs"), "fn f7() { edited }\n");
    let second = shadow.snapshot(CHAT).unwrap();
    assert_eq!(second.hashed, 1);
    assert!(second.changed);
    let edited = content(&shadow, &second, "src/file7.rs");
    assert_eq!(edited, b"fn f7() { edited }\n");

    let third = shadow.snapshot(CHAT).unwrap();
    assert_eq!((third.hashed, third.changed), (0, false));
    assert_eq!(
        third.commit, second.commit,
        "an unchanged tree keeps its commit"
    );
}

#[test]
fn the_project_is_left_as_it_was() {
    let fixture = Fixture::new();
    let root = fixture.project();
    write(&root.join(".gitignore"), "*.log\n");
    write(&root.join("src/a.rs"), "fn a() {}\n");
    write(&root.join("gone.txt"), "deleted after the commit\n");
    commit_all(&root);
    write(&root.join("src/a.rs"), "fn a() { changed }\n");
    fs::remove_file(root.join("gone.txt")).unwrap();
    write(&root.join("new.txt"), "untracked\n");
    write(&root.join("debug.log"), "ignored\n");
    let before = state(&root);

    let shadow = fixture.shadow();
    let snapshot = shadow.snapshot(CHAT).unwrap();
    shadow.snapshot("chat-2").unwrap();
    shadow.guards(&snapshot).unwrap();
    shadow.pack_loose().unwrap();
    drop(shadow);

    let after = state(&root);
    let changed: Vec<_> = before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .collect();
    assert!(changed.is_empty(), "changed in the project: {changed:?}");
    let expected = [".gitignore", "new.txt", "src/a.rs"];
    assert_eq!(paths(&fixture.shadow(), &snapshot), expected);
}

#[test]
fn a_subfolder_project_takes_the_rules_above_it() {
    let fixture = Fixture::new();
    let repo = fixture.project();
    write(&repo.join(".gitignore"), "*.tmp\n");
    write(&repo.join("app/main.rs"), "fn main() {}\n");
    write(&repo.join("lib/lib.rs"), "pub fn f() {}\n");
    let head = commit_all(&repo);
    write(&repo.join("app/scratch.tmp"), "left out from above\n");
    write(&repo.join("app/new.rs"), "fn new() {}\n");

    let shadow = Shadow::open(&fixture.shadow_dir(), &repo.join("app")).unwrap();
    let snapshot = shadow.snapshot(CHAT).unwrap();

    assert_eq!(shadow.subpath(), "app");
    let expected = [".gitignore", "app/main.rs", "app/new.rs", "lib/lib.rs"];
    assert_eq!(paths(&shadow, &snapshot), expected);
    assert_eq!(parent(&shadow, &snapshot), Some(head));
}

#[test]
fn line_endings_follow_the_project() {
    let cases: [(&str, &[u8]); 3] = [
        ("false", b"one\r\ntwo\r\n"),
        ("true", b"one\ntwo\n"),
        ("input", b"one\ntwo\n"),
    ];
    for (autocrlf, stored) in cases {
        let fixture = Fixture::new();
        let root = fixture.project();
        write(&root.join("README"), "readme\n");
        commit_all(&root);
        let mut config = Repository::open(&root).unwrap().config().unwrap();
        config.set_str("core.autocrlf", autocrlf).unwrap();
        write(&root.join("win.txt"), "one\r\ntwo\r\n");

        let shadow = fixture.shadow();
        let snapshot = shadow.snapshot(CHAT).unwrap();

        let content = content(&shadow, &snapshot, "win.txt");
        assert_eq!(content, stored, "core.autocrlf = {autocrlf}");
    }
}

#[test]
fn attributes_normalise_line_endings_too() {
    let fixture = Fixture::new();
    let root = fixture.project();
    write(&root.join(".gitattributes"), "*.txt text\n");
    commit_all(&root);
    write(&root.join("win.txt"), "one\r\ntwo\r\n");
    write(&root.join("win.dat"), "one\r\ntwo\r\n");

    let shadow = fixture.shadow();
    let snapshot = shadow.snapshot(CHAT).unwrap();

    assert_eq!(content(&shadow, &snapshot, "win.txt"), b"one\ntwo\n");
    assert_eq!(content(&shadow, &snapshot, "win.dat"), b"one\r\ntwo\r\n");
}

#[test]
fn a_folder_without_git_works() {
    let fixture = Fixture::new();
    let root = fixture.project();
    write(&root.join(".gitignore"), "*.o\n");
    write(&root.join("main.c"), "int main() {}\n");
    write(&root.join("main.o"), "object\n");
    // The shadow is in the folder here, and stays out of its own snapshots.
    let shadow = Shadow::open(&root.join("data/shadow.git"), &root).unwrap();

    let first = shadow.snapshot(CHAT).unwrap();
    assert_eq!(paths(&shadow, &first), [".gitignore", "main.c"]);
    assert_eq!(parent(&shadow, &first), None);

    write(&root.join("main.c"), "int main() { return 0; }\n");
    let second = shadow.snapshot(CHAT).unwrap();
    assert_eq!(parent(&shadow, &second), Some(first.commit));
    assert_eq!(second.hashed, 1);
}

#[test]
fn the_index_follows_head() {
    let fixture = Fixture::new();
    let root = fixture.project();
    write(&root.join(".gitignore"), "*.env\n");
    write(&root.join("a.txt"), "a\n");
    commit_all(&root);
    write(&root.join("notes.txt"), "untracked\n");
    let shadow = fixture.shadow();
    let first = shadow.snapshot(CHAT).unwrap();
    assert_eq!(paths(&shadow, &first), [".gitignore", "a.txt", "notes.txt"]);

    // A file committed though an ignore rule matches it comes in with HEAD,
    write(&root.join("example.env"), "KEY=\n");
    commit(&root, &["example.env"]);
    // a file an ignore rule matches now goes out, and so does a deleted one.
    write(&root.join(".gitignore"), "*.env\nnotes.txt\n");
    fs::remove_file(root.join("a.txt")).unwrap();
    let second = shadow.snapshot(CHAT).unwrap();

    assert_eq!(paths(&shadow, &second), [".gitignore", "example.env"]);
}

#[test]
fn the_parent_falls_back_when_head_cant_be_read() {
    let fixture = Fixture::new();
    let root = fixture.project();
    write(&root.join("a.txt"), "a\n");
    let head = commit_all(&root);
    let first = fixture.shadow().snapshot(CHAT).unwrap();

    // A shallow clone's history can't go with a snapshot,
    write(&root.join(".git/shallow"), format!("{head}\n"));
    write(&root.join("a.txt"), "b\n");
    let shadow = fixture.shadow();
    let second = shadow.snapshot(CHAT).unwrap();
    assert_eq!(parent(&shadow, &second), Some(first.commit));

    // nor can a `HEAD` whose commit is gone.
    fs::remove_file(root.join(".git/shallow")).unwrap();
    let hex = head.to_string();
    fs::remove_file(root.join(".git/objects").join(&hex[..2]).join(&hex[2..])).unwrap();
    write(&root.join("a.txt"), "c\n");
    let shadow = fixture.shadow();
    let third = shadow.snapshot(CHAT).unwrap();
    assert_eq!(parent(&shadow, &third), Some(second.commit));
    assert_eq!(content(&shadow, &third, "a.txt"), b"c\n");
}

#[test]
fn without_history_snapshots_go_on_each_other() {
    let fixture = Fixture::new();
    let root = fixture.project();
    write(&root.join("a.txt"), "a\n");
    let head = commit_all(&root);
    let shadow = fixture.shadow();
    let with = shadow.snapshot(CHAT).unwrap();
    assert_eq!(parent(&shadow, &with), Some(head));

    shadow.set_history(false).unwrap();
    let shadow = fixture.shadow();
    assert!(!shadow.with_history(), "the choice is kept for the project");
    let first = shadow.snapshot(CHAT).unwrap();
    assert_ne!(first.commit, with.commit);
    assert_eq!(parent(&shadow, &first), None);

    write(&root.join("a.txt"), "b\n");
    let second = shadow.snapshot(CHAT).unwrap();
    assert_eq!(parent(&shadow, &second), Some(first.commit));
}

#[test]
fn lfs_files_that_go_up_with_their_content_are_listed() {
    let fixture = Fixture::new();
    let root = fixture.project();
    // `HEAD` has the pointers; the work tree, the content git-lfs put there.
    let pointer = "version https://git-lfs.github.com/spec/v1\n\
                   oid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\n\
                   size 12\n";
    write(&root.join("assets/logo.png"), pointer);
    write(&root.join("assets/kept.bin"), pointer);
    write(
        &root.join(".gitattributes"),
        "*.png filter=lfs diff=lfs merge=lfs -text\n",
    );
    commit_all(&root);
    write(&root.join("assets/logo.png"), "\u{89}PNG logo\r\n");

    let shadow = fixture.shadow();
    let snapshot = shadow.snapshot(CHAT).unwrap();
    let guards = shadow.guards(&snapshot).unwrap();

    assert_eq!(guards.lfs, [("assets/logo.png".to_string(), 12)]);
    assert!(guards.large_files.is_empty());
}

#[test]
fn the_guards_list_what_a_first_push_would_carry() {
    let fixture = Fixture::new();
    let root = fixture.project();
    write(&root.join("ok.sh"), "echo ok\n");
    let head = commit_all(&root);
    // A submodule: a gitlink in `HEAD`, not checked out.
    let repo = Repository::open(&root).unwrap();
    let mut index = repo.index().unwrap();
    let zero = IndexTime::new(0, 0);
    index
        .add(&IndexEntry {
            ctime: zero,
            mtime: zero,
            dev: 0,
            ino: 0,
            mode: 0o160000,
            uid: 0,
            gid: 0,
            file_size: 0,
            id: head,
            flags: 0,
            flags_extended: 0,
            path: b"vendor/lib".to_vec(),
        })
        .unwrap();
    commit_index(&repo, &mut index);
    let big = fs::File::create(root.join("big.iso")).unwrap();
    big.set_len(LARGE_FILE + 1).unwrap();
    write(&root.join("crlf.sh"), "echo one\r\necho two\r\n");
    // A nested repository, without a commit yet.
    Repository::init(root.join("nested")).unwrap();
    write(&root.join("nested/inner.txt"), "inner\n");

    let shadow = fixture.shadow();
    let snapshot = shadow.snapshot(CHAT).unwrap();
    let guards = shadow.guards(&snapshot).unwrap();

    assert_eq!(snapshot.nested, ["nested"]);
    assert_eq!(paths(&shadow, &snapshot), ["big.iso", "crlf.sh", "ok.sh"]);
    assert_eq!(
        guards.large_files,
        [("big.iso".to_string(), LARGE_FILE + 1)]
    );
    assert_eq!(guards.nested, ["nested", "vendor/lib"]);
    assert_eq!(guards.crlf_scripts, ["crlf.sh"]);
    assert!(guards.history > 0);
}

#[test]
fn loose_objects_are_packed() {
    let fixture = Fixture::new();
    let root = fixture.project();
    write(&root.join("a.txt"), "a\n");
    commit_all(&root);
    write(&root.join("new.txt"), "new\n");
    let objects = fixture.shadow_dir().join("objects");
    let shadow = fixture.shadow();

    shadow.snapshot(CHAT).unwrap();
    assert_eq!(loose_objects(&objects), 0, "never packed, so packed now");

    write(&root.join("new.txt"), "newer\n");
    let snapshot = shadow.snapshot(CHAT).unwrap();
    assert!(loose_objects(&objects) > 0, "packed once a day");
    assert!(shadow.pack_loose().unwrap() > 0);
    assert_eq!(loose_objects(&objects), 0);

    let shadow = fixture.shadow();
    assert_eq!(content(&shadow, &snapshot, "new.txt"), b"newer\n");
}

#[test]
fn a_chat_id_has_to_make_a_ref() {
    let fixture = Fixture::new();
    write(&fixture.project().join("a.txt"), "a\n");
    let shadow = fixture.shadow();
    assert!(shadow.snapshot("../escape").is_err());
    assert!(shadow.snapshot("").is_err());
}

#[test]
fn the_rules_only_this_computer_has_go_to_the_server() {
    let fixture = Fixture::new();
    let root = fixture.project();
    write(&root.join("a.txt"), "a\n");
    commit_all(&root);
    write(
        &root.join(".git/info/exclude"),
        "# local notes\nsecret.txt\n\n/build-local/\n",
    );

    let excludes = fixture.shadow().excludes();

    assert_eq!(excludes, ["secret.txt", "/build-local/", "*.mine"]);
}

/// What a chat's overlay of the project lists after `change`, as
/// `overlay::chats::changes` would.
fn overlay(project: &Path, store: &Path, change: impl FnOnce(&OverlayFs)) -> Vec<OverlayChange> {
    let fs = OverlayFs::open(project, store, 1 << 30).unwrap();
    change(&fs);
    fs.flush();
    fs.changes()
        .into_iter()
        .filter(|c| !c.by_product())
        .collect()
}

/// Writes a file in the overlay: the chat's own copy of it.
fn put(fs: &OverlayFs, path: &str, content: &str) {
    let ino = fs.resolve(path).unwrap_or_else(|_| {
        let (parent, name) = fs.resolve_parent(path).unwrap();
        let made = fs.create(parent, name, Kind::File, 0o644, 0, 0, None);
        made.unwrap().ino
    });
    let empty = SetAttr {
        size: Some(0),
        ..SetAttr::default()
    };
    fs.setattr(ino, &empty).unwrap();
    fs.write(ino, 0, content.as_bytes()).unwrap();
}

/// A commit's files by path, with their contents.
fn tree_files(repo: &Repository, commit: Oid) -> Vec<(String, String)> {
    let tree = repo.find_commit(commit).unwrap().tree().unwrap();
    let mut files = Vec::new();
    tree.walk(TreeWalkMode::PreOrder, |dir, entry| {
        if entry.kind() == Some(ObjectType::Blob) {
            let blob = repo.find_blob(entry.id()).unwrap();
            let path = format!("{dir}{}", entry.name().unwrap());
            files.push((path, String::from_utf8_lossy(blob.content()).into_owned()));
        }
        TreeWalkResult::Ok
    })
    .unwrap();
    files.sort();
    files
}

fn pairs(files: &[(&str, &str)]) -> Vec<(String, String)> {
    let files = files.iter();
    files.map(|(p, c)| (p.to_string(), c.to_string())).collect()
}

#[test]
fn an_overlay_folds_into_its_landing_base_and_its_changes() {
    let fixture = Fixture::new();
    let root = fixture.project();
    write(&root.join("edited.txt"), "1\n2\n3\n");
    write(&root.join("deleted.txt"), "gone\n");
    write(&root.join("moved.txt"), "moving\n");
    write(&root.join("kept.txt"), "kept\n");
    commit_all(&root);
    let changes = overlay(&root, &fixture.dir.0.join("store"), |fs| {
        put(fs, "edited.txt", "ONE\n2\n3\n");
        fs.create(ROOT, "new", Kind::Dir, 0o755, 0, 0, None)
            .unwrap();
        put(fs, "new/file.txt", "new\n");
        fs.unlink(ROOT, "deleted.txt").unwrap();
        let to = fs.create(ROOT, "to", Kind::Dir, 0o755, 0, 0, None).unwrap();
        let moved = fs.rename(
            ROOT,
            "moved.txt",
            to.ino,
            "moved.txt",
            RenameMode::NoReplace,
        );
        moved.unwrap();
    });
    // The user edits the file after the chat did.
    write(&root.join("edited.txt"), "1\n2\nTHREE\n");

    let folded = fixture.shadow().fold(CHAT, &changes).unwrap();

    let repo = Repository::open(fixture.shadow_dir()).unwrap();
    let base = [
        ("deleted.txt", "gone\n"),
        ("edited.txt", "1\n2\n3\n"),
        ("kept.txt", "kept\n"),
        ("moved.txt", "moving\n"),
    ];
    assert_eq!(tree_files(&repo, folded.base), pairs(&base));
    let start = [
        ("edited.txt", "ONE\n2\n3\n"),
        ("kept.txt", "kept\n"),
        ("new/file.txt", "new\n"),
        ("to/moved.txt", "moving\n"),
    ];
    assert_eq!(tree_files(&repo, folded.start), pairs(&start));
    let start = repo.find_commit(folded.start).unwrap();
    assert_eq!(start.parent_id(0).unwrap(), folded.base);
}

#[test]
fn a_folded_overlay_stores_what_git_add_would() {
    let fixture = Fixture::new();
    let root = fixture.project();
    open_or_init(&root)
        .config()
        .unwrap()
        .set_bool("core.autocrlf", true)
        .unwrap();
    write(&root.join("a.txt"), "1\r\n2\r\n");
    commit_all(&root);
    let changes = overlay(&root, &fixture.dir.0.join("store"), |fs| {
        put(fs, "a.txt", "ONE\r\n2\r\n");
    });

    let folded = fixture.shadow().fold(CHAT, &changes).unwrap();

    let repo = Repository::open(fixture.shadow_dir()).unwrap();
    assert_eq!(
        tree_files(&repo, folded.base),
        pairs(&[("a.txt", "1\n2\n")])
    );
    assert_eq!(
        tree_files(&repo, folded.start),
        pairs(&[("a.txt", "ONE\n2\n")])
    );
}

#[test]
fn a_deleted_file_changed_since_is_settled_before_a_move() {
    let fixture = Fixture::new();
    let root = fixture.project();
    write(&root.join("a.txt"), "a\n");
    commit_all(&root);
    let changes = overlay(&root, &fixture.dir.0.join("store"), |fs| {
        fs.unlink(ROOT, "a.txt").unwrap();
    });
    write(&root.join("a.txt"), "changed here\n");

    let refused = fixture.shadow().fold(CHAT, &changes).unwrap_err();

    assert!(refused.contains("a.txt"), "{refused}");
}

/// A workspace of two folders for its composite: `api`, a repository of
/// its own, and `web`, a folder of the repository `shop` beside its `docs`
/// (K31), each with its shadow beside them.
fn shop(fixture: &Fixture) -> (Shadow, Shadow) {
    let (api, shop) = (fixture.dir.0.join("api"), fixture.dir.0.join("shop"));
    write(&api.join("src/a.ts"), "a\n");
    commit_all(&api);
    write(&shop.join("web/src/b.ts"), "b\n");
    write(&shop.join("docs/readme.md"), "docs\n");
    commit_all(&shop);
    let api = Shadow::open(&fixture.dir.0.join("api.git"), &api).unwrap();
    let web = Shadow::open(&fixture.dir.0.join("web.git"), &shop.join("web")).unwrap();
    (api, web)
}

/// The workspace's composite, reading both folders' shadows.
fn composite(fixture: &Fixture) -> Composite {
    let shadows = [fixture.dir.0.join("api.git"), fixture.dir.0.join("web.git")];
    Composite::open(&fixture.dir.0.join("shop.git"), &shadows).unwrap()
}

/// A folder's part of `chat`'s compose: its name, and its own tree in its
/// snapshot for the chat.
fn part(name: &str, shadow: &Shadow, chat: &str) -> (String, Oid) {
    let snapshot = shadow.snapshot(chat).unwrap();
    (name.to_string(), shadow.folder_tree(&snapshot).unwrap())
}

#[test]
fn composing_two_folders_trees_puts_each_under_its_name() {
    let fixture = Fixture::new();
    let (api, web) = shop(&fixture);
    // Not committed: in api's shadow, not its repository.
    write(&fixture.dir.0.join("api/src/new.ts"), "new\n");
    let composite = composite(&fixture);

    let parts = [part("api", &api, CHAT), part("web", &web, CHAT)];
    let commit = composite.compose(CHAT, &parts).unwrap();

    // Read through the folders' shadows, and through theirs the folders'
    // repositories; web's repository's other folder isn't the chat's.
    let expected = [
        ("api/src/a.ts", "a\n"),
        ("api/src/new.ts", "new\n"),
        ("web/src/b.ts", "b\n"),
    ];
    assert_eq!(tree_files(composite.repo(), commit), pairs(&expected));
    let name = format!("refs/rovik/snapshots/{CHAT}");
    let at = composite.repo().find_reference(&name).unwrap().target();
    assert_eq!(at, Some(commit));
}

#[test]
fn a_chats_compose_has_only_its_folders() {
    let fixture = Fixture::new();
    let (api, web) = shop(&fixture);
    let composite = composite(&fixture);
    let empty = composite.repo().treebuilder(None).unwrap().write().unwrap();

    let parts = [part("web", &web, "chat-2"), ("void".to_string(), empty)];
    let commit = composite.compose("chat-2", &parts).unwrap();

    assert_eq!(
        tree_files(composite.repo(), commit),
        pairs(&[("web/src/b.ts", "b\n")])
    );
    let tree = composite
        .repo()
        .find_commit(commit)
        .unwrap()
        .tree()
        .unwrap();
    let top: Vec<String> = tree.iter().map(|e| e.name().unwrap().to_string()).collect();
    assert_eq!(
        top,
        ["web"],
        "nor an empty folder, which git keeps no tree of"
    );
    let both = [part("api", &api, CHAT), part("web", &web, CHAT)];
    let other = composite.compose(CHAT, &both).unwrap();
    assert_ne!(other, commit, "each chat its own");
    assert!(composite.compose("../x", &both).is_err());
}

#[test]
fn an_unchanged_compose_keeps_its_commit() {
    let fixture = Fixture::new();
    let (api, web) = shop(&fixture);
    let composite = composite(&fixture);
    let parts = || [part("api", &api, CHAT), part("web", &web, CHAT)];

    let first = composite.compose(CHAT, &parts()).unwrap();
    assert_eq!(composite.compose(CHAT, &parts()).unwrap(), first);

    write(&fixture.dir.0.join("api/src/a.ts"), "a2\n");
    let second = composite.compose(CHAT, &parts()).unwrap();
    assert_ne!(second, first);
    let parent = composite.repo().find_commit(second).unwrap().parent_id(0);
    assert_eq!(parent.ok(), Some(first), "on the chat's previous one");
    let files = tree_files(composite.repo(), second);
    assert!(files.contains(&("api/src/a.ts".into(), "a2\n".into())));
}

#[test]
fn grafting_a_folders_result_copies_its_objects_into_the_folders_shadow_and_commits_on_its_current_tree(
) {
    use git2::build::TreeUpdateBuilder;
    use git2::FileMode;
    let fixture = Fixture::new();
    let (api, web) = shop(&fixture);
    let composite = composite(&fixture);
    let base =
        (composite.compose(CHAT, &[part("api", &api, CHAT), part("web", &web, CHAT)])).unwrap();
    // The server's result, as a fetch brings it into the composite: in
    // objects only the composite has.
    let repo = composite.repo();
    let changed = repo.blob(b"b2\n").unwrap();
    let added = repo.blob(b"added\n").unwrap();
    let a2 = repo.blob(b"a2\n").unwrap();
    let base_commit = repo.find_commit(base).unwrap();
    let mut update = TreeUpdateBuilder::new();
    update.upsert("web/src/b.ts", changed, FileMode::Blob);
    update.upsert("web/src/added.ts", added, FileMode::Blob);
    update.upsert("api/src/a.ts", a2, FileMode::Blob);
    let tree = update
        .create_updated(repo, &base_commit.tree().unwrap())
        .unwrap();
    let me = Signature::now("Test", "test@example.com").unwrap();
    let tree = repo.find_tree(tree).unwrap();
    let result = repo
        .commit(None, &me, &me, "result", &tree, &[&base_commit])
        .unwrap();
    let alone = Repository::open(fixture.dir.0.join("web.git")).unwrap();
    assert!(alone.find_blob(added).is_err(), "not there yet");

    let grafted = composite.graft("web", &web, CHAT, (base, result)).unwrap();

    // In web's shadow alone, which doesn't read the composite: web's part
    // in its place in its repository's tree, the rest as its snapshot has
    // it, on the snapshot's own tree.
    let alone = Repository::open(fixture.dir.0.join("web.git")).unwrap();
    let expected = [
        ("docs/readme.md", "docs\n"),
        ("web/src/added.ts", "added\n"),
        ("web/src/b.ts", "b2\n"),
    ];
    assert_eq!(tree_files(&alone, grafted), pairs(&expected));
    let snapshot = web.snapshot(CHAT).unwrap();
    let grafted_base = alone.find_commit(grafted).unwrap().parent(0).unwrap();
    assert_eq!(grafted_base.tree_id(), snapshot.tree);

    // A folder at its repository's root has its part as its tree.
    let grafted = composite.graft("api", &api, CHAT, (base, result)).unwrap();
    let alone = Repository::open(fixture.dir.0.join("api.git")).unwrap();
    assert_eq!(tree_files(&alone, grafted), pairs(&[("src/a.ts", "a2\n")]));
}

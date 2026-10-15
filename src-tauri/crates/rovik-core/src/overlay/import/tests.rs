use super::*;
use crate::overlay::land::LandError;
use crate::testing::{temp_dir, TempDir};
use git2::build::TreeUpdateBuilder;
use git2::{IndexAddOption, Signature};
use std::path::PathBuf;

const CHAT: &str = "imported-chat";

/// A project in a repository whose `HEAD` is the landing base, with the
/// overlays' data folder held for the test.
struct Fixture {
    project: PathBuf,
    repo: Repository,
    base: Oid,
    _dir: TempDir,
    _data: tokio::sync::OwnedMutexGuard<()>,
}

/// `files` committed at the repository's root, with `autocrlf` set as the
/// project's own setting: the machine's git settings don't count.
fn fixture(files: &[(&str, &str)], autocrlf: bool) -> Fixture {
    let dir = temp_dir();
    let data = chats::test_data(&dir.0.join("data"));
    let project = dir.0.join("project");
    let repo = Repository::init(&project).unwrap();
    let mut config = repo.config().unwrap();
    config.set_bool("core.autocrlf", autocrlf).unwrap();
    for (path, content) in files {
        write(&project.join(path), content);
    }
    let base = commit_all(&repo);
    Fixture {
        project,
        repo,
        base,
        _dir: dir,
        _data: data,
    }
}

impl Fixture {
    /// A result on the landing base: each path with its new content and
    /// mode, or deleted. The project's files stay as they are.
    fn result(&self, changes: &[(&str, Option<&str>, FileMode)]) -> Oid {
        let base = self.repo.find_commit(self.base).unwrap();
        let mut update = TreeUpdateBuilder::new();
        for (path, content, mode) in changes {
            match content {
                Some(content) => {
                    let blob = self.repo.blob(content.as_bytes()).unwrap();
                    update.upsert(*path, blob, *mode);
                }
                None => {
                    update.remove(*path);
                }
            }
        }
        let tree = update
            .create_updated(&self.repo, &base.tree().unwrap())
            .unwrap();
        let tree = self.repo.find_tree(tree).unwrap();
        let me = Signature::now("Rovik", "results@rovik.invalid").unwrap();
        (self.repo)
            .commit(None, &me, &me, "result", &tree, &[&base])
            .unwrap()
    }

    fn import(&self, result: Oid) -> Result<Imported, ImportError> {
        import(&self.repo, &self.project, "", &told(result, &[]))
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.project.join(path)).unwrap()
    }
}

/// The server's word on a result of the chat: its commit, with `marked`.
fn told(commit: Oid, marked: &[&str]) -> ChatResult {
    ChatResult {
        dialog_id: CHAT.into(),
        commit: commit.to_string(),
        seq: 1,
        outcome: "done".into(),
        pending: None,
        marked: marked.iter().map(|m| m.to_string()).collect(),
        unchecked: false,
    }
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn commit_all(repo: &Repository) -> Oid {
    let mut index = repo.index().unwrap();
    let all = ["*"].iter();
    index.add_all(all, IndexAddOption::DEFAULT, None).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let me = Signature::now("Me", "me@example.com").unwrap();
    let head = repo.head().and_then(|head| head.peel_to_commit()).ok();
    let parents: Vec<&git2::Commit> = head.iter().collect();
    repo.commit(Some("HEAD"), &me, &me, "base", &tree, &parents)
        .unwrap()
}

/// What the chat's store holds: each change's path and what it is.
fn store(dialog_id: &str) -> Vec<(String, &'static str)> {
    let changes = chats::changes(dialog_id).into_iter();
    changes
        .map(|change| {
            let what = match &change {
                OverlayChange::Delete { .. } => "deleted",
                OverlayChange::Write {
                    kind: Kind::Dir, ..
                } => "folder",
                OverlayChange::Write {
                    kind: Kind::Symlink,
                    ..
                } => "link",
                OverlayChange::Write { origin: None, .. } => "added",
                OverlayChange::Write {
                    perm_changed: true, ..
                } => "changed, with its mode",
                OverlayChange::Write { .. } => "changed",
            };
            (change.path().to_string(), what)
        })
        .collect()
}

const TEXT: FileMode = FileMode::Blob;

#[test]
fn a_result_becomes_the_chats_changes_and_lands() {
    let f = fixture(
        &[
            ("src/main.rs", "fn main() {}\n"),
            ("old.txt", "old\n"),
            ("run.sh", "echo hi\n"),
        ],
        false,
    );
    let result = f.result(&[
        ("src/main.rs", Some("fn main() { run() }\n"), TEXT),
        ("src/new/mod.rs", Some("pub fn run() {}\n"), TEXT),
        ("old.txt", None, TEXT),
        ("run.sh", Some("echo hi\n"), FileMode::BlobExecutable),
        // Build output in a project that ignores nothing: not the chat's
        // work, as locally.
        ("target/debug/app", Some("built\n"), TEXT),
    ]);

    let imported = f.import(result).unwrap();

    let changed = ["old.txt", "run.sh", "src/main.rs", "src/new/mod.rs"];
    assert_eq!(imported.changed, changed);
    assert!(imported.outside.is_empty());
    assert_eq!(imported.commit, result.to_string());
    assert_eq!(super::imported(CHAT), Some(imported));
    let expected = [
        ("old.txt", "deleted"),
        ("run.sh", "changed, with its mode"),
        ("src/main.rs", "changed"),
        ("src/new", "folder"),
        ("src/new/mod.rs", "added"),
    ];
    let expected: Vec<(String, &str)> = expected
        .into_iter()
        .map(|(path, what)| (path.to_string(), what))
        .collect();
    assert_eq!(store(CHAT), expected);
    assert_eq!(
        f.read("src/main.rs"),
        "fn main() {}\n",
        "nothing landed yet"
    );

    let landed = land_by_itself(CHAT).unwrap().unwrap();

    assert_eq!(landed.paths.len(), 5, "{landed:?}");
    assert_eq!(f.read("src/main.rs"), "fn main() { run() }\n");
    assert_eq!(f.read("src/new/mod.rs"), "pub fn run() {}\n");
    assert!(!f.project.join("old.txt").exists());
    assert!(!f.project.join("target").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(f.project.join("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
    assert!(!chats::exists(CHAT), "landed: the store is gone");
    assert!(chats::last_landing(CHAT).is_some(), "and Undo is there");
}

#[test]
fn line_endings_follow_the_projects_settings() {
    let f = fixture(&[("a.txt", "one\r\ntwo\r\n")], true);
    let base = f.repo.find_commit(f.base).unwrap().tree().unwrap();
    let stored = base.get_path(Path::new("a.txt")).unwrap();
    let blob = f.repo.find_blob(stored.id()).unwrap();
    assert_eq!(blob.content(), b"one\ntwo\n", "git stores it normalised");
    let result = f.result(&[
        ("a.txt", Some("one\nTWO\n"), TEXT),
        ("b.txt", Some("new\n"), TEXT),
    ]);

    f.import(result).unwrap();
    land_by_itself(CHAT).unwrap();

    assert_eq!(f.read("a.txt"), "one\r\nTWO\r\n");
    assert_eq!(f.read("b.txt"), "new\r\n");
}

#[test]
fn a_conflict_waits_and_lands_anyway() {
    let f = fixture(&[("a.txt", "1\n2\n3\n")], false);
    let result = f.result(&[("a.txt", Some("CHAT\n2\n3\n"), TEXT)]);
    write(&f.project.join("a.txt"), "PROJECT\n2\n3\n");

    f.import(result).unwrap();
    let waits = land_by_itself(CHAT);

    assert!(matches!(waits, Err(LandError::Conflicts(_))), "{waits:?}");
    let pending = chats::pending(CHAT).unwrap();
    assert_eq!(pending.conflicts.len(), 1);
    assert_eq!(pending.conflicts[0].path, "a.txt");
    assert_eq!(f.read("a.txt"), "PROJECT\n2\n3\n");

    // A newer result waits while the conflicts are resolved.
    let newer = f.result(&[("a.txt", Some("NEWER\n2\n3\n"), TEXT)]);
    assert_eq!(f.import(newer), Err(ImportError::Resolving));
    assert_eq!(super::imported(CHAT).unwrap().commit, result.to_string());

    chats::land_now(CHAT, true).unwrap();
    assert_eq!(f.read("a.txt"), "CHAT\n2\n3\n");
}

#[test]
fn a_merge_lands_by_itself_only_when_no_checks_would_run() {
    let f = fixture(&[("a.txt", "1\n2\n3\n")], false);
    let result = f.result(&[("a.txt", Some("ONE\n2\n3\n"), TEXT)]);
    // The project changed the file too, on other lines.
    write(&f.project.join("a.txt"), "1\n2\nTHREE\n");

    f.import(result).unwrap();
    let waits = land_by_itself(CHAT);

    assert!(matches!(waits, Err(LandError::Unchecked(_))), "{waits:?}");
    assert_eq!(f.read("a.txt"), "1\n2\nTHREE\n");
    let unchecked = ChatResult {
        unchecked: true,
        ..told(result, &[])
    };
    import(&f.repo, &f.project, "", &unchecked).unwrap();
    land_by_itself(CHAT).unwrap();
    assert_eq!(f.read("a.txt"), "ONE\n2\nTHREE\n");
}

#[test]
fn a_newer_result_replaces_one_that_waits() {
    let f = fixture(&[("a.txt", "a\n"), ("b.txt", "b\n")], false);
    let first = f.result(&[("a.txt", Some("first\n"), TEXT)]);
    f.import(first).unwrap();
    wait(CHAT, READY);

    let second = f.result(&[("b.txt", Some("second\n"), TEXT)]);
    f.import(second).unwrap();

    assert_eq!(store(CHAT), [("b.txt".to_string(), "changed")]);
    assert_eq!(chats::pending(CHAT).map(|p| p.reason), None);
    // A result that changes nothing here leaves nothing to land.
    f.import(f.result(&[])).unwrap();
    assert!(!chats::exists(CHAT));
}

#[test]
fn a_projects_folder_in_its_repository_takes_its_own_changes() {
    let f = fixture(&[("web/app.js", "app\n"), ("api/main.go", "main\n")], false);
    let result = f.result(&[
        ("web/app.js", Some("app2\n"), TEXT),
        ("api/main.go", Some("main2\n"), TEXT),
    ]);
    let web = f.project.join("web");

    let imported = import(&f.repo, &web, "web", &told(result, &[])).unwrap();

    assert_eq!(imported.changed, ["web/app.js"]);
    assert_eq!(imported.outside, ["api/main.go"]);
    assert_eq!(store(CHAT), [("app.js".to_string(), "changed")]);
    land_by_itself(CHAT).unwrap();
    assert_eq!(f.read("web/app.js"), "app2\n");
    assert_eq!(f.read("api/main.go"), "main\n");
}

#[test]
fn files_marked_on_the_server_wait_for_their_markers() {
    let f = fixture(&[("a.txt", "a\n")], false);
    let marked = "<<<<<<< project (now)\nA\n||||||| before\na\n=======\nB\n>>>>>>> this chat\n";
    let result = f.result(&[("a.txt", Some(marked), TEXT)]);

    import(&f.repo, &f.project, "", &told(result, &["a.txt"])).unwrap();

    assert_eq!(chats::markers_left(CHAT), ["a.txt"]);
    let waits = land_by_itself(CHAT);
    assert!(matches!(waits, Err(LandError::Failed(_))), "{waits:?}");
    assert_eq!(f.read("a.txt"), "a\n");
}

#[cfg(unix)]
#[test]
fn links_are_written_as_links() {
    let mut f = fixture(&[("a.txt", "a\n"), ("b.txt", "b\n")], false);
    std::os::unix::fs::symlink("a.txt", f.project.join("l")).unwrap();
    f.base = commit_all(&f.repo);
    let result = f.result(&[
        ("l", Some("b.txt"), FileMode::Link),
        ("m", Some("a.txt"), FileMode::Link),
    ]);

    f.import(result).unwrap();
    land_by_itself(CHAT).unwrap();

    let target = |name: &str| fs::read_link(f.project.join(name)).unwrap();
    assert_eq!(target("l"), Path::new("b.txt"));
    assert_eq!(target("m"), Path::new("a.txt"));
}

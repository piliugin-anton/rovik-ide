use super::branches::validate_branch_name;
use super::history::{compact_contains, git_log_blocking, git_log_scan, Generations};
#[cfg(unix)]
use super::remote::credential_fill;
use super::*;
use git2::{BranchType, Repository};

mod revert_tests {
    use super::*;
    use std::fs;

    /// A number that differs between runs of the tests. A temp path reused
    /// from an earlier run brings its trash with it (the system trash outlives
    /// the run), and a test about the trash would count what that run trashed
    /// from the same path as its own.
    fn run_stamp() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default()
    }

    fn repo_with_commit() -> (PathBuf, Repository) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "git-revert-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
            run_stamp()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
        fs::write(dir.join("b.txt"), "b\n").unwrap();
        let repo = Repository::init(&dir).unwrap();
        {
            let mut index = repo.index().unwrap();
            index
                .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = git2::Signature::now("t", "t@t").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }
        (dir, repo)
    }

    fn path(dir: &std::path::Path) -> String {
        dir.to_string_lossy().into_owned()
    }

    #[test]
    fn unstaged_change_goes_back_to_the_staged_version() {
        let (dir, _repo) = repo_with_commit();
        fs::write(dir.join("src/a.rs"), "fn a() { changed }\n").unwrap();
        assert_eq!(
            git_base_content_blocking(path(&dir), "src/a.rs".into())
                .unwrap()
                .as_deref(),
            Some("fn a() {}\n")
        );
        let out = git_revert_file_blocking(path(&dir), "src/a.rs".into()).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("src/a.rs")).unwrap(),
            "fn a() {}\n"
        );
        assert!(out.backed_up && !out.removed);
        assert!(
            !dir.join("src/a (before revert).rs").exists(),
            "the copy went to the trash"
        );
    }

    #[test]
    fn deleted_file_comes_back() {
        let (dir, _repo) = repo_with_commit();
        fs::remove_file(dir.join("b.txt")).unwrap();
        let out = git_revert_file_blocking(path(&dir), "b.txt".into()).unwrap();
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "b\n");
        assert!(!out.backed_up, "nothing to back up");
    }

    #[test]
    fn staged_only_change_goes_back_to_the_last_commit() {
        let (dir, repo) = repo_with_commit();
        fs::write(dir.join("b.txt"), "staged\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("b.txt")).unwrap();
        index.write().unwrap();
        assert_eq!(
            git_base_content_blocking(path(&dir), "b.txt".into())
                .unwrap()
                .as_deref(),
            Some("b\n")
        );
        git_revert_file_blocking(path(&dir), "b.txt".into()).unwrap();
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "b\n");
        let st = git_status_blocking(path(&dir)).unwrap();
        assert!(
            st.files.iter().all(|f| f.path != "b.txt"),
            "clean again: {:?}",
            st.files
        );
    }

    #[test]
    fn staged_and_unstaged_goes_back_to_the_staged_version() {
        let (dir, repo) = repo_with_commit();
        fs::write(dir.join("b.txt"), "staged\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("b.txt")).unwrap();
        index.write().unwrap();
        fs::write(dir.join("b.txt"), "staged\nand more\n").unwrap();
        git_revert_file_blocking(path(&dir), "b.txt".into()).unwrap();
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "staged\n");
    }

    #[test]
    fn new_files_go_to_the_trash() {
        let (dir, repo) = repo_with_commit();
        fs::write(dir.join("new.txt"), "x").unwrap();
        assert_eq!(
            git_base_content_blocking(path(&dir), "new.txt".into()).unwrap(),
            None
        );
        let out = git_revert_file_blocking(path(&dir), "new.txt".into()).unwrap();
        assert!(out.removed && !dir.join("new.txt").exists());
        // Added to the index but never committed: same, and out of the index.
        fs::write(dir.join("added.txt"), "y").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("added.txt")).unwrap();
        index.write().unwrap();
        git_revert_file_blocking(path(&dir), "added.txt".into()).unwrap();
        assert!(!dir.join("added.txt").exists());
        assert!(git_status_blocking(path(&dir)).unwrap().files.is_empty());
    }

    /// What the system trash holds from `dir`, by name: so a test can tell
    /// a file trashed from one deleted for good. Taken out of the trash.
    #[cfg(any(
        target_os = "windows",
        all(
            unix,
            not(target_os = "macos"),
            not(target_os = "ios"),
            not(target_os = "android")
        )
    ))]
    fn take_trashed_from(dir: &std::path::Path) -> Vec<String> {
        let dir = dir.canonicalize().unwrap();
        let items: Vec<trash::TrashItem> = trash::os_limited::list()
            .unwrap()
            .into_iter()
            .filter(|i| i.original_parent.canonicalize().ok().as_deref() == Some(dir.as_path()))
            .collect();
        // By stem: Windows' recycle bin names an item as Explorer shows it,
        // without a known extension.
        let mut names: Vec<String> = items
            .iter()
            .map(|i| {
                let name = std::path::Path::new(&i.name);
                name.file_stem()
                    .unwrap_or(name.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        trash::os_limited::purge_all(items).unwrap();
        names
    }

    /// Discard all sends a new file that was staged but never committed to
    /// the trash, as it does an untracked one: the hard reset alone deleted
    /// it for good.
    #[test]
    fn discard_all_trashes_a_staged_new_file_rather_than_deleting_it() {
        let (dir, repo) = repo_with_commit();
        fs::write(dir.join("added.txt"), "staged, never committed\n").unwrap();
        fs::write(dir.join("untracked.txt"), "u\n").unwrap();
        fs::write(dir.join("b.txt"), "changed\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("added.txt")).unwrap();
        index.write().unwrap();

        git_discard_all_blocking(path(&dir)).unwrap();

        assert!(!dir.join("added.txt").exists() && !dir.join("untracked.txt").exists());
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "b\n");
        assert!(git_status_blocking(path(&dir)).unwrap().files.is_empty());
        #[cfg(any(
            target_os = "windows",
            all(
                unix,
                not(target_os = "macos"),
                not(target_os = "ios"),
                not(target_os = "android")
            )
        ))]
        assert_eq!(take_trashed_from(&dir), ["added", "untracked"]);
    }

    /// A project that is a folder of a larger repository (a home folder kept
    /// in git, a monorepo's package): the panel lists, commits and discards
    /// only what's in the project's folder.
    #[test]
    fn a_project_inside_a_larger_repository_keeps_to_its_folder() {
        let (dir, repo) = repo_with_commit();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "t").unwrap();
        config.set_str("user.email", "t@t").unwrap();
        let project = path(&dir.join("src"));
        fs::write(dir.join("src/a.rs"), "fn a() { changed }\n").unwrap();
        fs::write(dir.join("src/new.rs"), "new\n").unwrap();
        fs::write(dir.join("b.txt"), "outside\n").unwrap();
        fs::write(dir.join("outside.txt"), "o\n").unwrap();

        let listed = |p: &str| -> Vec<String> {
            let mut files: Vec<String> = git_status_blocking(p.to_string())
                .unwrap()
                .files
                .into_iter()
                .map(|f| f.path)
                .collect();
            files.sort();
            files
        };
        assert_eq!(listed(&project), ["src/a.rs", "src/new.rs"]);

        // Commit takes the project's changes only.
        git_commit_blocking(project.clone(), "src only".into()).unwrap();
        assert!(listed(&project).is_empty());
        assert_eq!(listed(&path(&dir)), ["b.txt", "outside.txt"]);
        let head = repo.head().unwrap().peel_to_tree().unwrap();
        assert!(head.get_path(std::path::Path::new("src/new.rs")).is_ok());
        assert!(head.get_path(std::path::Path::new("outside.txt")).is_err());

        // Discard all throws away the project's changes only.
        fs::write(dir.join("src/a.rs"), "fn a() { again }\n").unwrap();
        fs::write(dir.join("src/n2.rs"), "n2\n").unwrap();
        git_discard_all_blocking(project.clone()).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("src/a.rs")).unwrap(),
            "fn a() { changed }\n"
        );
        assert!(!dir.join("src/n2.rs").exists());
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "outside\n");
        assert!(dir.join("outside.txt").exists());
        assert_eq!(listed(&path(&dir)), ["b.txt", "outside.txt"]);
        #[cfg(any(
            target_os = "windows",
            all(
                unix,
                not(target_os = "macos"),
                not(target_os = "ios"),
                not(target_os = "android")
            )
        ))]
        assert_eq!(take_trashed_from(&dir.join("src")), ["n2"]);
    }

    /// Without a commit yet, Discard all in a project folder empties the
    /// index of that folder's files only.
    #[test]
    fn discard_all_in_a_folder_of_a_repository_without_commits_keeps_the_rest_staged() {
        let dir = std::env::temp_dir().join(format!("git-unborn-scope-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("app")).unwrap();
        fs::write(dir.join("app/x.txt"), "x\n").unwrap();
        fs::write(dir.join("top.txt"), "t\n").unwrap();
        let repo = Repository::init(&dir).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();

        git_discard_all_blocking(path(&dir.join("app"))).unwrap();

        assert!(!dir.join("app/x.txt").exists());
        assert!(dir.join("top.txt").exists());
        let mut index = repo.index().unwrap();
        index.read(true).unwrap();
        let staged: Vec<String> = index
            .iter()
            .map(|e| String::from_utf8_lossy(&e.path).into_owned())
            .collect();
        assert_eq!(staged, ["top.txt"]);
        #[cfg(any(
            target_os = "windows",
            all(
                unix,
                not(target_os = "macos"),
                not(target_os = "ios"),
                not(target_os = "android")
            )
        ))]
        take_trashed_from(&dir.join("app"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// A file whose name is a pattern to libgit2 (`[id]`, `*`, `?`) is
    /// reverted alone: without a literal pathspec, `pages/[id].tsx` took
    /// `pages/i.tsx` with it, by force, with no copy in the trash.
    #[test]
    fn a_name_with_brackets_reverts_that_file_alone() {
        let (dir, repo) = repo_with_commit();
        fs::create_dir_all(dir.join("pages")).unwrap();
        fs::write(dir.join("pages/[id].tsx"), "dynamic\n").unwrap();
        fs::write(dir.join("pages/i.tsx"), "plain\n").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "pages", &tree, &[&head])
            .unwrap();
        // Both edited, one reverted: unstaged, then staged.
        for staged in [false, true] {
            fs::write(dir.join("pages/[id].tsx"), "dynamic edited\n").unwrap();
            fs::write(dir.join("pages/i.tsx"), "plain edited\n").unwrap();
            if staged {
                let mut index = repo.index().unwrap();
                index
                    .add_path(std::path::Path::new("pages/[id].tsx"))
                    .unwrap();
                index.add_path(std::path::Path::new("pages/i.tsx")).unwrap();
                index.write().unwrap();
            }
            git_revert_file_blocking(path(&dir), "pages/[id].tsx".into()).unwrap();
            assert_eq!(
                fs::read_to_string(dir.join("pages/[id].tsx")).unwrap(),
                "dynamic\n"
            );
            assert_eq!(
                fs::read_to_string(dir.join("pages/i.tsx")).unwrap(),
                "plain edited\n",
                "the other file keeps its edit (staged: {staged})"
            );
            git_revert_file_blocking(path(&dir), "pages/i.tsx".into()).unwrap();
        }
    }

    /// A copy restored from the trash under the backup's name isn't
    /// overwritten by the next revert: that one is numbered.
    #[test]
    fn a_backup_never_overwrites_an_earlier_one() {
        let (dir, _repo) = repo_with_commit();
        fs::write(dir.join("src/a (before revert).rs"), "restored\n").unwrap();
        fs::write(dir.join("src/a.rs"), "fn a() { changed }\n").unwrap();
        let out = git_revert_file_blocking(path(&dir), "src/a.rs".into()).unwrap();
        assert!(out.backed_up);
        assert_eq!(
            fs::read_to_string(dir.join("src/a (before revert).rs")).unwrap(),
            "restored\n"
        );
        assert!(
            !dir.join("src/a (before revert 2).rs").exists(),
            "went to the trash"
        );
    }

    /// Committing waits for a merge to be finished or aborted, and for
    /// conflicts to be resolved; discarding everything waits for the
    /// operation too (a rebase's hard reset would strand it).
    #[test]
    fn a_commit_or_a_discard_waits_for_an_operation_in_progress() {
        let (dir, repo) = repo_with_commit();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "t").unwrap();
        config.set_str("user.email", "t@t").unwrap();
        // A branch that changes b.txt, merged into a main that changed it too.
        let start = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("other", &start, false).unwrap();
        git_checkout_blocking(path(&dir), "other".into()).unwrap();
        fs::write(dir.join("b.txt"), "theirs\n").unwrap();
        git_commit_blocking(path(&dir), "theirs".into()).unwrap();
        let main = start.id();
        repo.set_head_detached(main).unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        git_create_branch_blocking(path(&dir), "main".into(), true, None).unwrap();
        fs::write(dir.join("b.txt"), "ours\n").unwrap();
        git_commit_blocking(path(&dir), "ours".into()).unwrap();
        let other = repo.find_branch("other", BranchType::Local).unwrap();
        let incoming = repo.reference_to_annotated_commit(other.get()).unwrap();
        repo.merge(&[&incoming], None, None).unwrap();
        assert_eq!(repo.state(), git2::RepositoryState::Merge);

        let err = git_commit_blocking(path(&dir), "merge".into()).unwrap_err();
        assert!(err.contains("merge is in progress"), "{err}");
        let err = git_discard_all_blocking(path(&dir)).unwrap_err();
        assert!(err.contains("merge is in progress"), "{err}");
        assert_eq!(repo.state(), git2::RepositoryState::Merge, "left as it was");
    }

    #[test]
    fn refuses_paths_outside_the_repository() {
        let (dir, _repo) = repo_with_commit();
        assert!(git_revert_file_blocking(path(&dir), "../etc/passwd".into()).is_err());
        assert!(git_revert_folder_blocking(path(&dir), "../etc".into()).is_err());
        assert!(git_revert_folder_blocking(path(&dir), "".into()).is_err());
        assert!(git_revert_folder_blocking(path(&dir), "/".into()).is_err());
    }

    #[test]
    fn a_folder_reverts_every_file_below_it_and_leaves_what_it_cant() {
        let (dir, repo) = repo_with_commit();
        fs::write(dir.join("src/a.rs"), "fn a() { changed }\n").unwrap();
        fs::write(dir.join("src/new.rs"), "new").unwrap();
        fs::create_dir_all(dir.join("src/deep")).unwrap();
        fs::write(dir.join("src/deep/n.txt"), "n").unwrap();
        fs::write(dir.join("b.txt"), "outside\n").unwrap();
        let out = git_revert_folder_blocking(path(&dir), "src/".into()).unwrap();
        assert_eq!(out.reverted, ["src/a.rs", "src/deep/n.txt", "src/new.rs"]);
        assert!(out.skipped.is_empty() && out.failed.is_empty(), "{out:?}");
        assert_eq!(
            fs::read_to_string(dir.join("src/a.rs")).unwrap(),
            "fn a() {}\n"
        );
        assert!(!dir.join("src/new.rs").exists() && !dir.join("src/deep/n.txt").exists());
        assert_eq!(
            fs::read_to_string(dir.join("b.txt")).unwrap(),
            "outside\n",
            "a file outside the folder is left alone"
        );

        // A staged rename in the folder is left as it is, and listed under
        // the path git lists it by (the old one, as its row in the Git tab);
        // the folder's other change goes back.
        fs::rename(dir.join("src/a.rs"), dir.join("src/c.rs")).unwrap();
        let mut index = repo.index().unwrap();
        index.remove_path(std::path::Path::new("src/a.rs")).unwrap();
        index.add_path(std::path::Path::new("src/c.rs")).unwrap();
        index.write().unwrap();
        fs::write(dir.join("src/d.rs"), "d").unwrap();
        let out = git_revert_folder_blocking(path(&dir), "src".into()).unwrap();
        assert_eq!(out.skipped, ["src/a.rs"]);
        assert_eq!(out.reverted, ["src/d.rs"]);
        assert!(dir.join("src/c.rs").exists() && !dir.join("src/d.rs").exists());
    }

    #[test]
    fn status_reports_workdir_and_staging() {
        let (dir, _repo) = repo_with_commit();
        fs::write(dir.join("src/a.rs"), "changed\n").unwrap();
        fs::write(dir.join("b.txt"), "changed\n").unwrap();
        let st = git_status_blocking(path(&dir.join("src"))).unwrap();
        // The repository's folder, however it's spelled there (macOS's /var
        // is /private/var; Windows' temporary folder has a short 8.3 name).
        let canonical = |p: &std::path::Path| fs::canonicalize(p).unwrap();
        assert_eq!(
            canonical(std::path::Path::new(&st.workdir)),
            canonical(&dir)
        );
        assert!(!st.workdir.ends_with(['/', '\\']), "{}", st.workdir);
        // Paths are the repository's; only the project's folder is listed.
        let a = st.files.iter().find(|f| f.path == "src/a.rs").unwrap();
        assert!(a.unstaged && !a.staged);
        assert!(!st.files.iter().any(|f| f.path == "b.txt"), "outside src");
    }

    fn stash_repo() -> PathBuf {
        let (dir, repo) = repo_with_commit();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "t").unwrap();
        config.set_str("user.email", "t@t").unwrap();
        fs::write(dir.join("src/a.rs"), "fn a() { stashed }\n").unwrap();
        fs::write(dir.join("new.txt"), "untracked stash me\n").unwrap();
        git_stash_push_blocking(path(&dir), Some("my stash".into())).unwrap();
        dir
    }

    #[test]
    fn stash_show_lists_tracked_and_untracked_files() {
        let dir = stash_repo();
        let files = git_stash_show_blocking(path(&dir), 0).unwrap();
        // What it touched, and nothing else of the branch: with untracked
        // files, the files it left alone (b.txt) aren't deleted ones.
        let listed: Vec<(&str, &str)> = files
            .iter()
            .map(|f| (f.path.as_str(), f.status.as_str()))
            .collect();
        assert_eq!(listed, [("new.txt", "A"), ("src/a.rs", "M")]);
        let n = files.iter().find(|f| f.path == "new.txt").unwrap();
        assert_eq!((n.additions, n.deletions), (1, 0));
        assert!(
            git_status_blocking(path(&dir)).unwrap().files.is_empty(),
            "tree is clean after stash"
        );
    }

    #[test]
    fn stash_file_content_returns_stashed_versions() {
        let dir = stash_repo();
        assert_eq!(
            git_stash_file_content_blocking(path(&dir), 0, "src/a.rs".into())
                .unwrap()
                .as_deref(),
            Some("fn a() { stashed }\n")
        );
        assert_eq!(
            git_stash_file_content_blocking(path(&dir), 0, "new.txt".into())
                .unwrap()
                .as_deref(),
            Some("untracked stash me\n")
        );
        // A file untouched by the stash still resolves from the stash commit.
        assert_eq!(
            git_stash_file_content_blocking(path(&dir), 0, "b.txt".into())
                .unwrap()
                .as_deref(),
            Some("b\n")
        );
    }

    #[test]
    fn stash_file_content_deleted_returns_none_and_bad_index_errors() {
        let (dir, repo) = repo_with_commit();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "t").unwrap();
        config.set_str("user.email", "t@t").unwrap();
        fs::remove_file(dir.join("b.txt")).unwrap();
        git_stash_push_blocking(path(&dir), None).unwrap();
        let files = git_stash_show_blocking(path(&dir), 0).unwrap();
        assert!(
            files.iter().any(|f| f.path == "b.txt" && f.status == "D"),
            "{files:?}"
        );
        assert_eq!(
            git_stash_file_content_blocking(path(&dir), 0, "b.txt".into()).unwrap(),
            None
        );
        assert!(git_stash_show_blocking(path(&dir), 42).is_err());
        assert!(git_stash_file_content_blocking(path(&dir), 0, "../etc/passwd".into()).is_err());
    }

    #[test]
    fn stash_file_preview_returns_content_and_base() {
        let dir = stash_repo();
        // Tracked modification: stashed content plus the base it changed from.
        let p = git_stash_file_preview_blocking(path(&dir), 0, "src/a.rs".into()).unwrap();
        assert_eq!(p.content.as_deref(), Some("fn a() { stashed }\n"));
        assert_eq!(p.base.as_deref(), Some("fn a() {}\n"));
        // Untracked file: new in the stash, so no base.
        let p = git_stash_file_preview_blocking(path(&dir), 0, "new.txt".into()).unwrap();
        assert_eq!(p.content.as_deref(), Some("untracked stash me\n"));
        assert_eq!(p.base, None);
        // Bad index and outside paths error.
        assert!(git_stash_file_preview_blocking(path(&dir), 42, "src/a.rs".into()).is_err());
        assert!(git_stash_file_preview_blocking(path(&dir), 0, "../etc/passwd".into()).is_err());
    }

    /// Second commit on top of the fixture: modifies a.rs, deletes b.txt,
    /// adds c.txt. Returns the new commit's full oid.
    fn second_commit(dir: &std::path::Path, repo: &Repository) -> String {
        fs::write(dir.join("src/a.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        fs::remove_file(dir.join("b.txt")).unwrap();
        fs::write(dir.join("c.txt"), "c\n").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index
            .remove_path(std::path::Path::new("b.txt"))
            .unwrap_or(());
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, "second", &tree, &[&head])
            .unwrap();
        oid.to_string()
    }

    #[test]
    fn commit_show_lists_changed_files_with_stats() {
        let (dir, repo) = repo_with_commit();
        let oid = second_commit(&dir, &repo);
        let files = git_commit_show_blocking(path(&dir), oid.clone(), None).unwrap();
        let a = files.iter().find(|f| f.path == "src/a.rs").unwrap();
        assert_eq!(a.status, "M");
        assert_eq!((a.additions, a.deletions), (1, 0));
        let b = files.iter().find(|f| f.path == "b.txt").unwrap();
        assert_eq!(b.status, "D");
        let c = files.iter().find(|f| f.path == "c.txt").unwrap();
        assert_eq!(c.status, "A");
        assert_eq!((c.additions, c.deletions), (1, 0));
        // Bad oid and outside paths error.
        assert!(git_commit_show_blocking(path(&dir), "not-an-oid".into(), None).is_err());
        assert!(git_commit_show_blocking(
            path(&dir),
            "0000000000000000000000000000000000000000".into(),
            None
        )
        .is_err());
    }

    #[test]
    fn commit_file_preview_returns_content_and_parent_base() {
        let (dir, repo) = repo_with_commit();
        let oid = second_commit(&dir, &repo);
        // Modified: content at the commit plus the parent's version.
        let p =
            git_commit_file_preview_blocking(path(&dir), oid.clone(), "src/a.rs".into()).unwrap();
        assert_eq!(p.content.as_deref(), Some("fn a() {}\nfn b() {}\n"));
        assert_eq!(p.base.as_deref(), Some("fn a() {}\n"));
        // Deleted: no content, parent base present.
        let p = git_commit_file_preview_blocking(path(&dir), oid.clone(), "b.txt".into()).unwrap();
        assert_eq!(p.content, None);
        assert_eq!(p.base.as_deref(), Some("b\n"));
        // Added: content present, no base.
        let p = git_commit_file_preview_blocking(path(&dir), oid.clone(), "c.txt".into()).unwrap();
        assert_eq!(p.content.as_deref(), Some("c\n"));
        assert_eq!(p.base, None);
        assert!(git_commit_file_preview_blocking(path(&dir), oid, "../etc/passwd".into()).is_err());
    }

    #[test]
    fn commit_file_preview_bases_a_renamed_file_on_its_old_path() {
        let (dir, repo) = repo_with_commit();
        let commit_all = |message: &str| {
            let mut index = repo.index().unwrap();
            index
                .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.update_all(["*"].iter(), None).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = git2::Signature::now("t", "t@t").unwrap();
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&head])
                .unwrap()
                .to_string()
        };
        let before: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        fs::write(dir.join("notes.txt"), &before).unwrap();
        commit_all("add notes");
        // Moved into docs/ with a line added.
        fs::create_dir_all(dir.join("docs")).unwrap();
        fs::remove_file(dir.join("notes.txt")).unwrap();
        fs::write(dir.join("docs/notes.txt"), format!("{before}more\n")).unwrap();
        let oid = commit_all("move notes");
        let p = git_commit_file_preview_blocking(path(&dir), oid, "docs/notes.txt".into()).unwrap();
        assert_eq!(p.content, Some(format!("{before}more\n")));
        // What it was before the move: only "more" is new.
        assert_eq!(p.base, Some(before));
    }

    #[test]
    fn commit_show_initial_commit_reports_everything_added() {
        let (dir, _repo) = repo_with_commit();
        let repo = Repository::open(&dir).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let oid = head.id().to_string();
        let files = git_commit_show_blocking(path(&dir), oid.clone(), None).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|f| f.status == "A"));
        let p = git_commit_file_preview_blocking(path(&dir), oid, "b.txt".into()).unwrap();
        assert_eq!(p.content.as_deref(), Some("b\n"));
        assert_eq!(p.base, None);
    }

    #[test]
    fn git_log_query_matches_message_filename_and_content() {
        let (dir, repo) = repo_with_commit();
        // Distinctive commit: message, filename, and symbol share "widget"
        // but are otherwise unrelated to the fixture's "init"/a.rs/b.txt.
        fs::write(dir.join("unique_widget.rs"), "fn unique_foobar_xyz() {}\n").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_path(std::path::Path::new("unique_widget.rs"))
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let oid2 = repo
            .commit(Some("HEAD"), &sig, &sig, "add widget", &tree, &[&head])
            .unwrap()
            .to_string();
        // Baseline: no query returns both commits, newest first.
        let all = git_log_blocking(path(&dir), Some(20), None, None).unwrap();
        assert_eq!(all.commits.len(), 2);
        assert_eq!(all.commits[0].oid, oid2);
        assert!(!all.has_more);
        // Message match.
        let by_msg = git_log_blocking(path(&dir), Some(20), Some("widget".into()), None).unwrap();
        assert_eq!(by_msg.commits.len(), 1);
        assert_eq!(by_msg.commits[0].oid, oid2);
        // Filename match: partial basename.
        let by_file =
            git_log_blocking(path(&dir), Some(20), Some("unique_widget".into()), None).unwrap();
        assert_eq!(by_file.commits.len(), 1);
        assert_eq!(by_file.commits[0].oid, oid2);
        // Content match: identifier and its partial form.
        let by_symbol =
            git_log_blocking(path(&dir), Some(20), Some("unique_foobar_xyz".into()), None).unwrap();
        assert_eq!(by_symbol.commits.len(), 1);
        assert_eq!(by_symbol.commits[0].oid, oid2);
        let by_partial =
            git_log_blocking(path(&dir), Some(20), Some("foobar".into()), None).unwrap();
        assert_eq!(by_partial.commits.len(), 1);
        assert_eq!(by_partial.commits[0].oid, oid2);
        // AND semantics: every term must match somewhere.
        let both = git_log_blocking(
            path(&dir),
            Some(20),
            Some("widget unique_foobar".into()),
            None,
        )
        .unwrap();
        assert_eq!(both.commits.len(), 1);
        let none = git_log_blocking(
            path(&dir),
            Some(20),
            Some("widget nosuchthingzzz".into()),
            None,
        )
        .unwrap();
        assert!(none.commits.is_empty());
        // Empty/blank query behaves like no query.
        let blank = git_log_blocking(path(&dir), Some(20), Some("   ".into()), None).unwrap();
        assert_eq!(blank.commits.len(), 2);
    }

    #[test]
    fn git_log_reports_file_counts_and_commit_show_flags_matches() {
        let (dir, repo) = repo_with_commit();
        let oid2 = second_commit(&dir, &repo);
        // No query: file counts present, no matched count.
        let all = git_log_blocking(path(&dir), Some(20), None, None).unwrap();
        let newest = all.commits.iter().find(|e| e.oid == oid2).unwrap();
        assert_eq!(newest.file_count, 3);
        assert_eq!(newest.matched_file_count, None);
        // Query matching one filename: matched count narrows to that file.
        let filtered = git_log_blocking(path(&dir), Some(20), Some("c.txt".into()), None).unwrap();
        let entry = filtered.commits.iter().find(|e| e.oid == oid2).unwrap();
        assert_eq!(entry.file_count, 3);
        assert_eq!(entry.matched_file_count, Some(1));
        // Per-file flags: only c.txt matches, sorted first.
        let files =
            git_commit_show_blocking(path(&dir), oid2.clone(), Some("c.txt".into())).unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, "c.txt");
        assert!(files[0].matched);
        assert_eq!(files[0].match_kind.as_deref(), Some("filename"));
        assert!(!files[1].matched && !files[2].matched);
        // No query: all files matched, no kinds.
        let plain = git_commit_show_blocking(path(&dir), oid2, None).unwrap();
        assert!(plain.iter().all(|f| f.matched));
        assert!(plain.iter().all(|f| f.match_kind.is_none()));
    }

    #[test]
    fn git_log_pages_through_limit_and_offset() {
        let (dir, repo) = repo_with_commit();
        // Three more commits on top of the fixture (4 total).
        for i in 0..3 {
            fs::write(dir.join(format!("p{i}.txt")), format!("p{i}\n")).unwrap();
            let mut index = repo.index().unwrap();
            index
                .add_path(std::path::Path::new(&format!("p{i}.txt")))
                .unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = git2::Signature::now("t", "t@t").unwrap();
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            repo.commit(
                Some("HEAD"),
                &sig,
                &sig,
                &format!("page {i}"),
                &tree,
                &[&head],
            )
            .unwrap();
        }
        // Page 1: newest 2, more remain.
        let first = git_log_blocking(path(&dir), Some(2), None, None).unwrap();
        assert_eq!(first.commits.len(), 2);
        assert!(first.has_more);
        assert_eq!(first.next_offset, 2);
        // Page 2 resumes where page 1 stopped: next 2, exhausted.
        let second = git_log_blocking(path(&dir), Some(2), None, Some(first.next_offset)).unwrap();
        assert_eq!(second.commits.len(), 2);
        assert!(!second.has_more);
        assert_ne!(first.commits[0].oid, second.commits[0].oid);
        // Query pages the same way: "page-1" is one compact term
        // (compact "page1" matches "page 1" with the space ignored,
        // but not "page 0"/"page 2" — and never a hex oid).
        let q1 = git_log_blocking(path(&dir), Some(20), Some("page-1".into()), None).unwrap();
        assert_eq!(q1.commits.len(), 1);
        let q2 = git_log_blocking(
            path(&dir),
            Some(20),
            Some("page-1".into()),
            Some(q1.next_offset),
        )
        .unwrap();
        assert!(q2.commits.is_empty());
    }

    #[test]
    fn git_log_single_char_skips_content_scan() {
        let (dir, repo) = repo_with_commit();
        fs::write(dir.join("zz_top.txt"), "qqqqqq\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("zz_top.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "add zz", &tree, &[&head])
            .unwrap();
        // "q" appears only in content: single-char terms match
        // metadata/filenames, so this must NOT match.
        let page = git_log_blocking(path(&dir), Some(20), Some("q".into()), None).unwrap();
        assert!(page.commits.is_empty());
        // The filename itself still matches.
        let by_name = git_log_blocking(path(&dir), Some(20), Some("zz_top".into()), None).unwrap();
        assert_eq!(by_name.commits.len(), 1);
    }

    #[test]
    fn compact_contains_matches_compacted_forms() {
        assert!(compact_contains("get_user-name", "getusername"));
        assert!(compact_contains("fn getUserName() {", "getusername"));
        assert!(!compact_contains("fn other() {", "getusername"));
        assert!(!compact_contains("abc", ""));
    }

    /// Stage everything (deletions included) and commit it on HEAD.
    fn commit_all(repo: &Repository, message: &str) -> String {
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.update_all(["*"].iter(), None).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&head])
            .unwrap()
            .to_string()
    }

    fn found(dir: &std::path::Path, query: &str) -> Vec<String> {
        git_log_blocking(path(dir), Some(20), Some(query.into()), None)
            .unwrap()
            .commits
            .into_iter()
            .map(|c| c.summary)
            .collect()
    }

    #[test]
    fn git_log_searches_the_whole_message() {
        let (dir, repo) = repo_with_commit();
        fs::write(dir.join("notes.txt"), "plain\n").unwrap();
        commit_all(&repo, "Add notes\n\nMentions the frobnicator in the body.");
        assert_eq!(found(&dir, "frobnicator"), ["Add notes"]);
    }

    #[test]
    fn symbol_terms_dont_match_on_one_leftover_character() {
        let (dir, repo) = repo_with_commit();
        // The fixture's `src/a.rs` has a "c" in its path; "c++" compacts to
        // "c" and used to match it (and nearly every other commit).
        assert!(found(&dir, "c++").is_empty());
        assert!(found(&dir, "c#").is_empty());
        // A real "c++" in the content still matches by its raw form.
        fs::write(dir.join("README.md"), "Built with c++ and care.\n").unwrap();
        commit_all(&repo, "readme");
        assert_eq!(found(&dir, "c++"), ["readme"]);
        // Two compact characters are enough for the separator-blind form.
        fs::write(dir.join("x.txt"), "set a_b here\n").unwrap();
        commit_all(&repo, "x");
        assert_eq!(found(&dir, "a-b"), ["x"]);
    }

    #[test]
    fn the_no_newline_marker_isnt_content() {
        let (dir, repo) = repo_with_commit();
        // No trailing newline: git adds a "\\ No newline at end of file" line.
        fs::write(dir.join("notes.txt"), "plain text").unwrap();
        commit_all(&repo, "Add notes");
        assert!(found(&dir, "newline").is_empty());
        assert_eq!(found(&dir, "plain"), ["Add notes"]);
    }

    #[test]
    fn the_badge_counts_files_like_the_expanded_list() {
        let (dir, repo) = repo_with_commit();
        for n in ["one", "two", "three"] {
            fs::write(
                dir.join(format!("w_{n}.rs")),
                format!("fn widget_{n}() {{}}\n"),
            )
            .unwrap();
        }
        // The message alone satisfies the query; the files match by content.
        let oid = commit_all(&repo, "widget update");
        let page = git_log_blocking(path(&dir), Some(20), Some("widget".into()), None).unwrap();
        let entry = page.commits.iter().find(|c| c.oid == oid).unwrap();
        let shown = git_commit_show_blocking(path(&dir), oid, Some("widget".into())).unwrap();
        let matched = shown.iter().filter(|f| f.matched).count();
        assert_eq!(matched, 3);
        assert_eq!(
            entry.matched_file_count,
            Some(matched),
            "badge and expanded list agree"
        );
    }

    #[test]
    fn a_renamed_file_isnt_all_new_content() {
        let (dir, repo) = repo_with_commit();
        fs::write(
            dir.join("old_name.rs"),
            "fn renamed_marker_xyz() {}\nfn other() {}\nfn third() {}\n",
        )
        .unwrap();
        commit_all(&repo, "add file");
        fs::rename(dir.join("old_name.rs"), dir.join("new_name.rs")).unwrap();
        let moved = commit_all(&repo, "move file");
        // Only the commit that wrote the line matches its content.
        assert_eq!(found(&dir, "renamed_marker_xyz"), ["add file"]);
        let files = git_commit_show_blocking(path(&dir), moved, None).unwrap();
        assert_eq!(files.len(), 1, "{files:?}");
        assert_eq!(
            (files[0].path.as_str(), files[0].status.as_str()),
            ("new_name.rs", "R")
        );
    }

    #[test]
    fn a_superseded_search_stops_scanning() {
        let (dir, _repo) = repo_with_commit();
        let page = git_log_scan(
            path(&dir),
            Some(20),
            Some("init".into()),
            None,
            None,
            &|| true,
        )
        .unwrap();
        assert!(page.commits.is_empty() && page.scanned == 0);
        assert!(page.has_more, "stopped, not exhausted");
        let page = git_log_scan(
            path(&dir),
            Some(20),
            Some("init".into()),
            None,
            None,
            &|| false,
        )
        .unwrap();
        assert_eq!(page.commits.len(), 1);
    }

    #[test]
    fn two_repositories_history_reads_dont_stop_each_other() {
        // A count of its own, not the app's: no other test shares it.
        let latest = Generations::new();
        let api = latest.begin("/w/api");
        let web = latest.begin("/w/web");
        assert!(
            latest.is_latest("/w/api", api),
            "another repository's read leaves it going"
        );
        assert!(latest.is_latest("/w/web", web));
        let again = latest.begin("/w/api");
        assert!(
            !latest.is_latest("/w/api", api),
            "a newer read of its own repository stops it"
        );
        assert!(latest.is_latest("/w/api", again));
        assert!(latest.is_latest("/w/web", web), "and only its own");
    }

    fn page_from(dir: &std::path::Path, offset: Option<usize>, tip: Option<&str>) -> GitLogPage {
        git_log_scan(path(dir), Some(2), None, offset, tip, &|| false).unwrap()
    }

    #[test]
    fn a_commit_made_while_paging_doesnt_repeat_or_skip_one() {
        let (dir, repo) = repo_with_commit();
        for i in 0..3 {
            fs::write(dir.join(format!("p{i}.txt")), format!("p{i}\n")).unwrap();
            commit_all(&repo, &format!("page {i}"));
        }
        // 4 commits: page 2 of 2 each.
        let first = page_from(&dir, None, None);
        let tip = first.tip.clone().expect("the walk's start");
        assert_eq!(
            tip,
            repo.head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id()
                .to_string()
        );
        // Someone commits from a terminal before "load more".
        fs::write(dir.join("late.txt"), "late\n").unwrap();
        commit_all(&repo, "made meanwhile");
        let second = page_from(&dir, Some(first.next_offset), Some(&tip));
        assert_eq!(
            second.tip.as_deref(),
            Some(tip.as_str()),
            "still the same session"
        );
        let summaries: Vec<&str> = first
            .commits
            .iter()
            .chain(&second.commits)
            .map(|c| c.summary.as_str())
            .collect();
        // Every commit of the session once, in order; the new one isn't in it.
        assert_eq!(summaries, ["page 2", "page 1", "page 0", "init"]);
        assert!(!second.has_more);
        // Counting from HEAD instead (what paging did before) repeats one.
        let shifted = page_from(&dir, Some(first.next_offset), None);
        assert_eq!(
            shifted.commits[0].summary, "page 1",
            "the old behaviour: page 1 again"
        );
        // A fresh session sees the new commit.
        assert_eq!(
            page_from(&dir, None, None).commits[0].summary,
            "made meanwhile"
        );
    }

    #[test]
    fn a_tip_that_no_longer_exists_starts_over_from_head() {
        let (dir, _repo) = repo_with_commit();
        let gone = "0123456789abcdef0123456789abcdef01234567";
        let page = page_from(&dir, Some(5), Some(gone));
        // Fell back to HEAD, from the start: the old offset meant nothing here.
        assert_ne!(page.tip.as_deref(), Some(gone));
        assert_eq!(page.commits.len(), 1);
        assert_eq!(page.commits[0].summary, "init");
    }
}

mod branch_tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "git-branch-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn p(dir: &Path) -> String {
        dir.to_string_lossy().into_owned()
    }

    fn commit_all(repo: &Repository, message: &str) -> git2::Oid {
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.update_all(["*"].iter(), None).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .unwrap()
    }

    /// A repository on `main` with one commit (f.txt = "one").
    fn repo_on_main(tag: &str) -> (PathBuf, Repository) {
        let dir = temp_dir(tag);
        let repo = Repository::init(&dir).unwrap();
        {
            let mut c = repo.config().unwrap();
            c.set_str("user.name", "t").unwrap();
            c.set_str("user.email", "t@t").unwrap();
        }
        fs::write(dir.join("f.txt"), "one\n").unwrap();
        commit_all(&repo, "start");
        if repo.head().unwrap().shorthand().ok() != Some("main") {
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            repo.branch("main", &head, true).unwrap();
            repo.set_head("refs/heads/main").unwrap();
        }
        (dir, repo)
    }

    fn head_branch(repo: &Repository) -> Option<String> {
        let head = repo.head().unwrap();
        head.is_branch()
            .then(|| head.shorthand().unwrap().to_string())
    }

    fn tip(repo: &Repository, refname: &str) -> git2::Oid {
        repo.refname_to_id(refname).unwrap()
    }

    fn upstream_of(repo: &Repository, branch: &str) -> Option<String> {
        repo.find_branch(branch, BranchType::Local)
            .unwrap()
            .upstream()
            .ok()
            .map(|u| u.name().unwrap().unwrap().to_string())
    }

    /// `origin` (a bare repository) with `main` and `feature` pushed to
    /// it and fetched back, so `origin/main` and `origin/feature` are
    /// known; the local `feature` is deleted again. Both at the start commit.
    fn repo_with_origin(tag: &str) -> (PathBuf, Repository, PathBuf) {
        let (dir, repo) = repo_on_main(tag);
        let remote = temp_dir(&format!("{tag}-origin"));
        Repository::init_bare(&remote).unwrap();
        repo.remote("origin", &p(&remote)).unwrap();
        assert_eq!(git_push_blocking(p(&dir)).unwrap().result, "pushed");
        git_create_branch_blocking(p(&dir), "feature".into(), true, None).unwrap();
        assert_eq!(git_push_blocking(p(&dir)).unwrap().result, "pushed");
        git_checkout_blocking(p(&dir), "main".into()).unwrap();
        git_delete_branch_blocking(p(&dir), "feature".into()).unwrap();
        git_fetch_blocking(p(&dir)).unwrap();
        assert!(repo
            .find_branch("origin/feature", BranchType::Remote)
            .is_ok());
        (dir, repo, remote)
    }

    fn head_of(dir: &Path) -> GitHead {
        git_head_blocking(p(dir)).unwrap()
    }

    fn on(branch: &str, main_branch: Option<&str>) -> GitHead {
        GitHead {
            branch: Some(branch.into()),
            main_branch: main_branch.map(Into::into),
        }
    }

    #[test]
    fn the_main_branch_is_the_one_origin_head_names() {
        let (dir, repo, _remote) = repo_with_origin("head-origin");
        // What a clone records of the remote's default branch.
        repo.reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
            true,
            "clone",
        )
        .unwrap();
        assert_eq!(head_of(&dir), on("main", Some("main")));

        // A remote whose default is another branch: main is just a branch.
        repo.reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/feature",
            true,
            "set-head",
        )
        .unwrap();
        assert_eq!(head_of(&dir), on("main", Some("feature")));
    }

    #[test]
    fn without_origin_head_the_main_branch_is_where_git_init_starts_then_main_then_master() {
        let (dir, repo) = repo_on_main("head-local");
        // The repository's own setting, so this machine's doesn't count.
        repo.config()
            .unwrap()
            .set_str("init.defaultBranch", "trunk")
            .unwrap();
        let start = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("master", &start, true).unwrap();
        assert_eq!(
            head_of(&dir),
            on("main", Some("main")),
            "main before master"
        );

        repo.branch("trunk", &start, false).unwrap();
        assert_eq!(head_of(&dir), on("main", Some("trunk")));

        git_checkout_blocking(p(&dir), "master".into()).unwrap();
        git_delete_branch_blocking(p(&dir), "trunk".into()).unwrap();
        git_delete_branch_blocking(p(&dir), "main".into()).unwrap();
        assert_eq!(head_of(&dir), on("master", Some("master")));

        git_create_branch_blocking(p(&dir), "work".into(), true, None).unwrap();
        git_delete_branch_blocking(p(&dir), "master".into()).unwrap();
        assert_eq!(
            head_of(&dir),
            on("work", None),
            "none of them: no main branch"
        );
    }

    #[test]
    fn head_is_on_no_branch_when_detached_or_before_the_first_commit() {
        let (dir, repo) = repo_on_main("head-detached");
        // The repository's own setting, so this machine's doesn't count: with
        // master there (CI's Windows runner), the master `repo_on_main` leaves
        // beside main would be the main branch.
        repo.config()
            .unwrap()
            .set_str("init.defaultBranch", "main")
            .unwrap();
        let start = tip(&repo, "refs/heads/main");
        repo.set_head_detached(start).unwrap();
        assert_eq!(
            head_of(&dir),
            GitHead {
                branch: None,
                main_branch: Some("main".into())
            }
        );

        let empty = temp_dir("head-unborn");
        Repository::init(&empty).unwrap();
        assert_eq!(
            head_of(&empty),
            GitHead {
                branch: None,
                main_branch: None
            }
        );
    }

    #[test]
    fn head_outside_a_repository_says_so() {
        let err = git_head_blocking(p(&temp_dir("head-no-repo"))).unwrap_err();
        // The chat panel tells this apart from a repository it can't read.
        assert!(err.starts_with("Not a git repository"), "{err}");
    }

    #[test]
    fn branch_names_follow_gits_rules() {
        for ok in ["feature", "feature/x", "v1.2", "a-b_c", "ünïcode"] {
            assert_eq!(validate_branch_name(ok).unwrap(), ok, "{ok}");
        }
        for bad in [
            "", " ", "HEAD", "@", "-x", "/x", "x/", "x.", "a..b", "a//b", "a@{1}", "a b", "a~b",
            "a^b", "a:b", "a?b", "a*b", "a[b", "a\\b", ".hidden", "a/.b", "a.lock", "a.lock/b",
            "a\x07b",
        ] {
            assert!(
                validate_branch_name(bad).is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn a_branch_from_a_local_branch_starts_at_its_commit_without_tracking() {
        let (dir, repo) = repo_on_main("from-local");
        let start = tip(&repo, "refs/heads/main");
        fs::write(dir.join("f.txt"), "two\n").unwrap();
        commit_all(&repo, "two");
        git_create_branch_blocking(
            p(&dir),
            "old".into(),
            false,
            Some("refs/heads/main~1".into()),
        )
        .unwrap();
        assert_eq!(tip(&repo, "refs/heads/old"), start);
        git_create_branch_blocking(p(&dir), "copy".into(), false, Some("old".into())).unwrap();
        assert_eq!(tip(&repo, "refs/heads/copy"), start);
        assert_eq!(upstream_of(&repo, "copy"), None);
        // Still on main, with its tree: nothing was checked out.
        assert_eq!(head_branch(&repo).as_deref(), Some("main"));
        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "two\n");
    }

    #[test]
    fn a_branch_from_a_remote_branch_tracks_it_only_under_the_same_name() {
        let (dir, repo, _remote) = repo_with_origin("from-remote");
        let remote_tip = tip(&repo, "refs/remotes/origin/feature");

        git_create_branch_blocking(
            p(&dir),
            "feature".into(),
            false,
            Some("origin/feature".into()),
        )
        .unwrap();
        assert_eq!(tip(&repo, "refs/heads/feature"), remote_tip);
        assert_eq!(
            upstream_of(&repo, "feature").as_deref(),
            Some("origin/feature"),
            "`feature` from `origin/feature` pulls from and pushes to it"
        );

        git_create_branch_blocking(p(&dir), "mine".into(), false, Some("origin/feature".into()))
            .unwrap();
        assert_eq!(tip(&repo, "refs/heads/mine"), remote_tip);
        assert_eq!(
            upstream_of(&repo, "mine"),
            None,
            "the panel pushes `mine` to origin/mine; tracking origin/feature would keep it 'ahead' for ever"
        );
        // Its first push sets the upstream, as for any new branch.
        git_checkout_blocking(p(&dir), "mine".into()).unwrap();
        assert_eq!(git_push_blocking(p(&dir)).unwrap().result, "pushed");
        assert_eq!(upstream_of(&repo, "mine").as_deref(), Some("origin/mine"));
    }

    /// A new branch has no remote branch until its first push, which makes
    /// one and tracks it: the panel asks to publish it then (GitPanel.tsx).
    #[test]
    fn a_new_branch_has_no_upstream_until_its_first_push() {
        let (dir, repo, remote) = repo_with_origin("publish");
        let status = git_status_blocking(p(&dir)).unwrap();
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!(status.push_remote.as_deref(), Some("origin"));

        git_create_branch_blocking(p(&dir), "topic".into(), true, None).unwrap();
        fs::write(dir.join("f.txt"), "topic\n").unwrap();
        commit_all(&repo, "topic");
        let status = git_status_blocking(p(&dir)).unwrap();
        assert_eq!(status.branch, "topic");
        assert_eq!(status.upstream, None);
        assert_eq!(status.push_remote.as_deref(), Some("origin"));
        assert_eq!(status.ahead, 0, "nothing to be ahead of");

        assert_eq!(git_push_blocking(p(&dir)).unwrap().result, "pushed");
        let status = git_status_blocking(p(&dir)).unwrap();
        assert_eq!(status.upstream.as_deref(), Some("origin/topic"));
        assert_eq!((status.ahead, status.behind), (0, 0));
        let origin = Repository::open_bare(&remote).unwrap();
        assert_eq!(
            tip(&origin, "refs/heads/topic"),
            tip(&repo, "refs/heads/topic")
        );

        // No remote: nowhere to publish it.
        repo.remote_delete("origin").unwrap();
        let status = git_status_blocking(p(&dir)).unwrap();
        assert_eq!((status.upstream, status.push_remote), (None, None));
    }

    #[test]
    fn a_branch_in_the_list_beats_a_tag_of_the_same_name() {
        let (dir, repo) = repo_on_main("branch-vs-tag");
        let first = tip(&repo, "refs/heads/main");
        repo.branch("rel", &repo.find_commit(first).unwrap(), false)
            .unwrap();
        fs::write(dir.join("f.txt"), "two\n").unwrap();
        let second = commit_all(&repo, "two");
        // `git rev-parse rel` would pick the tag (refs/tags before refs/heads).
        repo.tag_lightweight("rel", &repo.find_object(second, None).unwrap(), false)
            .unwrap();
        git_create_branch_blocking(p(&dir), "from-rel".into(), false, Some("rel".into())).unwrap();
        assert_eq!(tip(&repo, "refs/heads/from-rel"), first);
    }

    #[test]
    fn create_and_switch_carries_uncommitted_work_over() {
        let (dir, repo) = repo_on_main("carry");
        fs::write(dir.join("f.txt"), "edited\n").unwrap();
        git_create_branch_blocking(p(&dir), "work".into(), true, Some("main".into())).unwrap();
        assert_eq!(head_branch(&repo).as_deref(), Some("work"));
        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "edited\n");
    }

    #[test]
    fn a_switch_the_working_folder_refuses_leaves_no_branch_behind() {
        let (dir, repo) = repo_on_main("refused");
        git_create_branch_blocking(p(&dir), "other".into(), true, None).unwrap();
        fs::write(dir.join("f.txt"), "two\n").unwrap();
        commit_all(&repo, "two on other");
        git_checkout_blocking(p(&dir), "main".into()).unwrap();
        // An edit to the file the branches differ in: switching would lose it.
        fs::write(dir.join("f.txt"), "local edit\n").unwrap();

        let err = git_create_branch_blocking(p(&dir), "new".into(), true, Some("other".into()))
            .unwrap_err();
        assert!(err.contains("Checkout failed"), "{err}");
        assert!(
            repo.find_branch("new", BranchType::Local).is_err(),
            "the branch that couldn't be switched to isn't left behind"
        );
        assert_eq!(head_branch(&repo).as_deref(), Some("main"));
        assert_eq!(
            fs::read_to_string(dir.join("f.txt")).unwrap(),
            "local edit\n"
        );
        // The same name works once the edit is out of the way.
        fs::write(dir.join("f.txt"), "one\n").unwrap();
        git_create_branch_blocking(p(&dir), "new".into(), true, Some("other".into())).unwrap();
        assert_eq!(head_branch(&repo).as_deref(), Some("new"));
        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "two\n");
    }

    #[test]
    fn a_missing_start_point_is_an_error_and_creates_nothing() {
        let (dir, repo) = repo_on_main("missing");
        let err =
            git_create_branch_blocking(p(&dir), "x".into(), true, Some("nope".into())).unwrap_err();
        assert!(err.contains("nope"), "{err}");
        assert!(repo.find_branch("x", BranchType::Local).is_err());
        assert_eq!(head_branch(&repo).as_deref(), Some("main"));
    }

    #[test]
    fn checking_out_a_remote_branch_creates_a_tracking_branch() {
        let (dir, repo, _remote) = repo_with_origin("remote-checkout");
        git_checkout_blocking(p(&dir), "origin/feature".into()).unwrap();
        assert_eq!(head_branch(&repo).as_deref(), Some("feature"));
        assert_eq!(
            upstream_of(&repo, "feature").as_deref(),
            Some("origin/feature")
        );
        assert_eq!(
            tip(&repo, "refs/heads/feature"),
            tip(&repo, "refs/remotes/origin/feature")
        );
    }

    #[test]
    fn checking_out_a_remote_branch_switches_to_the_local_one_it_already_has() {
        let (dir, repo, _remote) = repo_with_origin("remote-vs-local");
        // A local `feature`, one commit past origin/feature.
        git_checkout_blocking(p(&dir), "origin/feature".into()).unwrap();
        fs::write(dir.join("f.txt"), "local two\n").unwrap();
        let local_tip = commit_all(&repo, "two");
        git_checkout_blocking(p(&dir), "main".into()).unwrap();
        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "one\n");

        git_checkout_blocking(p(&dir), "origin/feature".into()).unwrap();
        assert_eq!(head_branch(&repo).as_deref(), Some("feature"));
        assert_eq!(tip(&repo, "refs/heads/feature"), local_tip);
        // The working folder is at the local branch's commit, not the
        // remote's: nothing shows up as changed.
        assert_eq!(
            fs::read_to_string(dir.join("f.txt")).unwrap(),
            "local two\n"
        );
        assert!(git_status_blocking(p(&dir)).unwrap().files.is_empty());
    }

    #[test]
    fn checking_out_a_tag_detaches_head() {
        let (dir, repo) = repo_on_main("tag");
        let first = tip(&repo, "refs/heads/main");
        repo.tag_lightweight("v1", &repo.find_object(first, None).unwrap(), false)
            .unwrap();
        fs::write(dir.join("f.txt"), "two\n").unwrap();
        commit_all(&repo, "two");
        git_checkout_blocking(p(&dir), "v1".into()).unwrap();
        let head = repo.head().unwrap();
        assert!(!head.is_branch(), "HEAD must not point at refs/tags/v1");
        assert!(repo.head_detached().unwrap());
        assert_eq!(head.target(), Some(first));
        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "one\n");
        assert!(git_status_blocking(p(&dir))
            .unwrap()
            .branch
            .starts_with("detached@"));
    }

    #[test]
    fn fetch_and_push_go_to_the_branches_own_remote() {
        let (dir, repo, origin) = repo_with_origin("two-remotes");
        let fork = temp_dir("two-remotes-fork");
        Repository::init_bare(&fork).unwrap();
        repo.remote("fork", &p(&fork)).unwrap();
        {
            let mut c = repo.config().unwrap();
            c.set_str("branch.main.remote", "fork").unwrap();
            c.set_str("branch.main.merge", "refs/heads/main").unwrap();
        }
        let before = tip(&repo, "refs/heads/main");
        fs::write(dir.join("f.txt"), "two\n").unwrap();
        let after = commit_all(&repo, "two");

        assert_eq!(git_push_blocking(p(&dir)).unwrap().result, "pushed");
        let fork_tip = Repository::open_bare(&fork)
            .unwrap()
            .refname_to_id("refs/heads/main")
            .unwrap();
        assert_eq!(fork_tip, after, "pushed to the remote the branch tracks");
        let origin_tip = Repository::open_bare(&origin)
            .unwrap()
            .refname_to_id("refs/heads/main")
            .unwrap();
        assert_eq!(origin_tip, before, "origin wasn't touched");

        git_fetch_blocking(p(&dir)).unwrap();
        assert_eq!(tip(&repo, "refs/remotes/fork/main"), after);
        assert_eq!(tip(&repo, "refs/remotes/origin/main"), before);
        assert_eq!(compute_ahead_behind(&repo), Some((0, 0)));
    }
}

#[cfg(unix)]
mod credential_tests {
    use super::credential_fill;
    use std::path::Path;
    use std::process::Command;

    /// git with `config` for its only settings, outside any repository.
    fn git(config: &Path) -> Command {
        let mut git = Command::new("git");
        git.env("GIT_CONFIG_GLOBAL", config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(config.parent().unwrap());
        git
    }

    #[test]
    fn a_remembered_credential_is_what_git_answers() {
        let dir = std::env::temp_dir().join(format!("git-credential-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let helper = dir.join("helper.gitconfig");
        std::fs::write(
            &helper,
            "[credential]\n\thelper = \"!f() { echo username=ann; echo password=s3cret; }; f\"\n",
        )
        .unwrap();
        let nothing = dir.join("nothing.gitconfig");
        std::fs::write(&nothing, "").unwrap();
        let url = "https://git.example.com/team/app.git";

        assert_eq!(
            credential_fill(git(&helper), url, None),
            Some(Some(("ann".to_string(), "s3cret".to_string())))
        );
        // Nothing remembered: no answer, and no prompt waiting for one.
        assert_eq!(credential_fill(git(&nothing), url, Some("ann")), Some(None));
        // No git: git2 asks the helpers instead.
        assert_eq!(
            credential_fill(Command::new("/nonexistent/git"), url, None),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

mod identity_tests {
    use super::*;

    /// A repository's own identity is the one its commits get, whatever the
    /// global config says (the local level outranks every other).
    #[test]
    fn a_commit_takes_the_repositorys_own_identity() {
        let dir = std::env::temp_dir().join(format!("git-identity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = Repository::init(&dir).unwrap();
        let mut local = repo
            .config()
            .unwrap()
            .open_level(git2::ConfigLevel::Local)
            .unwrap();
        local.set_str("user.name", "Local").unwrap();
        local.set_str("user.email", "local@example.com").unwrap();
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        git_commit_blocking(dir.to_string_lossy().into_owned(), "first".into()).unwrap();
        let head = head_commit(&repo).unwrap();
        for who in [head.author(), head.committer()] {
            assert_eq!(who.name().unwrap(), "Local");
            assert_eq!(who.email().unwrap(), "local@example.com");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// The repository's git hooks, run around the panel's commit, push and
/// switch as git runs them (hooks.rs). Unix only: they're executable files.
#[cfg(unix)]
mod hook_tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "git-hooks-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn p(dir: &Path) -> String {
        dir.to_string_lossy().into_owned()
    }

    /// A repository on `main` with one commit, and a.txt changed since.
    fn repo(tag: &str) -> (PathBuf, Repository) {
        let dir = temp_dir(tag);
        let repo = Repository::init(&dir).unwrap();
        {
            let mut c = repo.config().unwrap();
            c.set_str("user.name", "t").unwrap();
            c.set_str("user.email", "t@t").unwrap();
            // Whatever the machine's global config says.
            c.set_str("core.hooksPath", ".git/hooks").unwrap();
        }
        fs::write(dir.join("a.txt"), "one\n").unwrap();
        git_commit_blocking(p(&dir), "start".into()).unwrap();
        if repo.head().unwrap().shorthand().ok() != Some("main") {
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            repo.branch("main", &head, true).unwrap();
            repo.set_head("refs/heads/main").unwrap();
        }
        fs::write(dir.join("a.txt"), "two\n").unwrap();
        (dir, repo)
    }

    /// Hook `name` in `folder`, a shell script running `body`.
    fn hook_in(folder: &Path, name: &str, body: &str) {
        fs::create_dir_all(folder).unwrap();
        let file = folder.join(name);
        fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn hook(dir: &Path, name: &str, body: &str) {
        hook_in(&dir.join(".git/hooks"), name, body);
    }

    fn head_message(repo: &Repository) -> String {
        head_commit(repo).unwrap().message().unwrap().to_string()
    }

    #[test]
    fn a_failing_pre_commit_stops_the_commit_and_says_why() {
        let (dir, repo) = repo("pre-commit-fails");
        let before = head_commit(&repo).unwrap().id();
        hook(
            &dir,
            "pre-commit",
            "echo checking\necho 'lint: 2 errors' >&2\nexit 1",
        );
        let err = git_commit_blocking(p(&dir), "change".into()).unwrap_err();
        assert_eq!(
            err,
            "Not committed: the pre-commit hook failed (exit code 1):\nchecking\nlint: 2 errors"
        );
        assert_eq!(head_commit(&repo).unwrap().id(), before);
    }

    #[test]
    fn what_pre_commit_stages_is_committed() {
        let (dir, repo) = repo("pre-commit-stages");
        // A formatter's fix, staged as lint-staged stages it.
        hook(
            &dir,
            "pre-commit",
            "echo formatted > a.txt && git add a.txt",
        );
        git_commit_blocking(p(&dir), "change".into()).unwrap();
        let tree = head_commit(&repo).unwrap().tree().unwrap();
        let blob = tree_blob(Some(&tree), Path::new("a.txt")).unwrap();
        assert_eq!(blob_text(&repo, blob, "a.txt").unwrap(), "formatted\n");
        assert!(git_status_blocking(p(&dir)).unwrap().files.is_empty());
    }

    #[test]
    fn the_message_hooks_get_the_message_and_can_change_it() {
        let (dir, repo) = repo("message-hooks");
        hook(
            &dir,
            "prepare-commit-msg",
            "echo \"$1 $2\" > prepare-args\nprintf '[T-1] %s' \"$(cat \"$1\")\" > \"$1\"",
        );
        hook(&dir, "commit-msg", "grep -q '^\\[T-' \"$1\"");
        git_commit_blocking(p(&dir), "fix it".into()).unwrap();
        assert_eq!(head_message(&repo), "[T-1] fix it\n");
        assert_eq!(
            fs::read_to_string(dir.join("prepare-args")).unwrap(),
            ".git/COMMIT_EDITMSG message\n"
        );
    }

    #[test]
    fn a_commit_msg_that_refuses_the_message_stops_the_commit() {
        let (dir, repo) = repo("commit-msg-refuses");
        let before = head_commit(&repo).unwrap().id();
        hook(
            &dir,
            "commit-msg",
            "grep -qE '^(feat|fix): ' \"$1\" || { echo 'use feat: or fix:'; exit 1; }",
        );
        let err = git_commit_blocking(p(&dir), "stuff".into()).unwrap_err();
        assert!(err.contains("commit-msg hook failed") && err.ends_with("use feat: or fix:"));
        assert_eq!(head_commit(&repo).unwrap().id(), before);
        git_commit_blocking(p(&dir), "fix: stuff".into()).unwrap();
        assert_eq!(head_message(&repo), "fix: stuff\n");
    }

    #[test]
    fn post_commit_runs_after_and_its_failure_changes_nothing() {
        let (dir, repo) = repo("post-commit");
        hook(
            &dir,
            "post-commit",
            "git log -1 --format=%s > after\nexit 1",
        );
        git_commit_blocking(p(&dir), "change".into()).unwrap();
        assert_eq!(head_message(&repo), "change");
        assert_eq!(fs::read_to_string(dir.join("after")).unwrap(), "change\n");
    }

    #[test]
    fn a_hook_that_isnt_executable_is_skipped_as_git_skips_it() {
        let (dir, _repo) = repo("not-executable");
        hook(&dir, "pre-commit", "exit 1");
        let file = dir.join(".git/hooks/pre-commit");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        git_commit_blocking(p(&dir), "change".into()).unwrap();
    }

    #[test]
    fn a_script_without_a_shebang_runs_with_sh() {
        let (dir, _repo) = repo("no-shebang");
        let file = dir.join(".git/hooks/pre-commit");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "echo no shebang; exit 3\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        let err = git_commit_blocking(p(&dir), "change".into()).unwrap_err();
        assert!(err.ends_with("(exit code 3):\nno shebang"), "{err}");
    }

    #[test]
    fn core_hooks_path_is_where_the_hooks_are() {
        let (dir, repo) = repo("hooks-path");
        // Relative: from the working folder, as husky sets it.
        repo.config()
            .unwrap()
            .set_str("core.hooksPath", ".husky/_")
            .unwrap();
        hook(&dir, "pre-commit", "exit 0");
        hook_in(
            &dir.join(".husky/_"),
            "pre-commit",
            "echo from husky; exit 1",
        );
        let err = git_commit_blocking(p(&dir), "change".into()).unwrap_err();
        assert!(err.ends_with("from husky"), "{err}");
    }

    #[test]
    fn pre_push_gets_the_remote_and_the_refs_and_can_stop_the_push() {
        let (dir, repo) = repo("pre-push");
        let remote = temp_dir("pre-push-origin");
        Repository::init_bare(&remote).unwrap();
        repo.remote("origin", &p(&remote)).unwrap();
        let first = head_commit(&repo).unwrap().id();
        hook(
            &dir,
            "pre-push",
            "echo \"$1 $2\" > push-args\ncat > push-refs",
        );
        assert_eq!(git_push_blocking(p(&dir)).unwrap().result, "pushed");
        assert_eq!(
            fs::read_to_string(dir.join("push-args")).unwrap(),
            format!("origin {}\n", p(&remote))
        );
        let zero = git2::Oid::ZERO_SHA1;
        assert_eq!(
            fs::read_to_string(dir.join("push-refs")).unwrap(),
            format!("refs/heads/main {first} refs/heads/main {zero}\n")
        );

        hook(&dir, "pre-push", "echo 'tests failed' >&2; exit 1");
        git_commit_blocking(p(&dir), "second".into()).unwrap();
        let err = git_push_blocking(p(&dir)).unwrap_err();
        assert_eq!(
            err,
            "Not pushed: the pre-push hook failed (exit code 1):\ntests failed"
        );
        let remote_tip = Repository::open_bare(&remote)
            .unwrap()
            .refname_to_id("refs/heads/main")
            .unwrap();
        assert_eq!(remote_tip, first, "nothing was sent");
    }

    #[test]
    fn no_verify_skips_the_checks_and_runs_the_other_hooks() {
        let (dir, repo) = repo("no-verify-commit");
        hook(&dir, "pre-commit", "exit 1");
        hook(&dir, "commit-msg", "exit 1");
        hook(&dir, "prepare-commit-msg", "echo prepare >> .git/ran");
        hook(&dir, "post-commit", "echo post >> .git/ran");
        assert!(git_commit_blocking(p(&dir), "wip".into()).is_err());
        git_commit_blocking_with(p(&dir), "wip".into(), true).unwrap();
        assert_eq!(head_message(&repo), "wip\n");
        assert_eq!(
            fs::read_to_string(dir.join(".git/ran")).unwrap(),
            "prepare\npost\n",
            "the refused try stopped at pre-commit, before the message hooks"
        );
    }

    #[test]
    fn no_verify_pushes_past_a_failing_pre_push() {
        let (dir, repo) = repo("no-verify-push");
        let remote = temp_dir("no-verify-push-origin");
        Repository::init_bare(&remote).unwrap();
        repo.remote("origin", &p(&remote)).unwrap();
        hook(&dir, "pre-push", "exit 1");
        assert!(git_push_blocking(p(&dir)).is_err());
        assert_eq!(
            git_push_blocking_with(p(&dir), true).unwrap().result,
            "pushed"
        );
        let remote_tip = Repository::open_bare(&remote)
            .unwrap()
            .refname_to_id("refs/heads/main")
            .unwrap();
        assert_eq!(remote_tip, head_commit(&repo).unwrap().id());
    }

    #[test]
    fn post_checkout_runs_after_a_switch() {
        let (dir, repo) = repo("post-checkout");
        git_commit_blocking(p(&dir), "two".into()).unwrap();
        let main = head_commit(&repo).unwrap().id();
        git_create_branch_blocking(p(&dir), "old".into(), false, Some("main~1".into())).unwrap();
        let old = repo.refname_to_id("refs/heads/old").unwrap();
        hook(
            &dir,
            "post-checkout",
            "echo \"$@\" > .git/post-checkout-args",
        );
        git_checkout_blocking(p(&dir), "old".into()).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join(".git/post-checkout-args")).unwrap(),
            format!("{main} {old} 1\n")
        );
    }
}

mod scopes_tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "git-scopes-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    /// A repository at a new folder, with a committed `a.ts` in each of
    /// `folders`.
    fn repo_with(folders: &[&str]) -> PathBuf {
        let dir = temp("repo");
        for folder in folders {
            fs::create_dir_all(dir.join(folder)).unwrap();
            fs::write(dir.join(folder).join("a.ts"), "a\n").unwrap();
        }
        let repo = Repository::init(&dir).unwrap();
        {
            let mut index = repo.index().unwrap();
            index
                .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = git2::Signature::now("t", "t@t").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }
        dir
    }

    fn path(dir: &std::path::Path) -> String {
        dir.to_string_lossy().into_owned()
    }

    fn scopes(list: &[&str]) -> Option<Vec<String>> {
        Some(list.iter().map(|scope| scope.to_string()).collect())
    }

    fn changed(status: &GitStatus) -> Vec<String> {
        let mut paths: Vec<String> = status.files.iter().map(|file| file.path.clone()).collect();
        paths.sort();
        paths
    }

    #[test]
    fn a_status_with_two_scopes_lists_both_and_nothing_beside() {
        let dir = repo_with(&["app/src", "web/src", "infra"]);
        for file in ["app/src/a.ts", "web/src/a.ts", "infra/a.ts"] {
            fs::write(dir.join(file), "changed\n").unwrap();
        }
        let status = git_status_in(path(&dir), scopes(&["app", "web"])).unwrap();
        assert_eq!(changed(&status), ["app/src/a.ts", "web/src/a.ts"]);
        // The line counts stay in scope too.
        assert_eq!(
            status
                .files
                .iter()
                .map(|file| file.additions)
                .sum::<usize>(),
            2
        );
        // No scopes given: the whole working tree, as a project's panel reads it.
        assert_eq!(changed(&git_status_in(path(&dir), None).unwrap()).len(), 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_line_counts_of_a_scope_count_that_folder_only() {
        let dir = repo_with(&["app", "app2"]);
        fs::write(dir.join("app/a.ts"), "one\n# two\n").unwrap();
        fs::write(dir.join("app2/a.ts"), "one\n# two\n# three\n").unwrap();
        let repo = Repository::open(&dir).unwrap();
        // Counted directly: the status lists only the scope's files, so this
        // is what proves the counts are the scope's too (counting the whole
        // repository passes the status test alone).
        let stats = workdir_line_stats_in(&repo, &["app".to_string()]);
        let counted: Vec<&String> = stats.keys().collect();
        assert_eq!(counted, ["app/a.ts"], "nothing of app2's is counted");
        assert_eq!(stats["app/a.ts"], (2, 1), "its own lines alone");
        // The whole working folder counts both, as a folder that is the
        // repository reads it.
        assert_eq!(workdir_line_stats_in(&repo, &[String::new()]).len(), 2);
        // Two scopes count both, as one repository's section does.
        let both = workdir_line_stats_in(&repo, &["app".to_string(), "app2".to_string()]);
        assert_eq!(both.len(), 2, "{both:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_scope_does_not_take_a_folder_sharing_its_first_letters() {
        let dir = repo_with(&["app", "app2"]);
        fs::write(dir.join("app/a.ts"), "one\n").unwrap();
        fs::write(dir.join("app2/a.ts"), "two\n").unwrap();
        let status = git_status_in(path(&dir), scopes(&["app"])).unwrap();
        assert_eq!(changed(&status), ["app/a.ts"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_commit_in_scopes_commits_those_folders_and_leaves_the_rest_changed() {
        let dir = repo_with(&["app", "web"]);
        fs::write(dir.join("app/a.ts"), "one\n").unwrap();
        fs::write(dir.join("web/a.ts"), "two\n").unwrap();
        git_commit_in(path(&dir), "two folders".into(), false, scopes(&["app"])).unwrap();
        let status = git_status_in(path(&dir), None).unwrap();
        assert_eq!(changed(&status), ["web/a.ts"]);
        assert_eq!(
            fs::read_to_string(dir.join("app/a.ts")).unwrap(),
            "one\n",
            "the commit kept its folder's change"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_discard_in_scopes_throws_away_those_folders_only() {
        let dir = repo_with(&["app", "web"]);
        fs::write(dir.join("app/a.ts"), "one\n").unwrap();
        fs::write(dir.join("web/a.ts"), "two\n").unwrap();
        git_discard_all_in(path(&dir), scopes(&["app"])).unwrap();
        assert_eq!(fs::read_to_string(dir.join("app/a.ts")).unwrap(), "a\n");
        assert_eq!(
            fs::read_to_string(dir.join("web/a.ts")).unwrap(),
            "two\n",
            "the other folder keeps its change"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rename_staged_in_a_terminal_is_committed_as_it_is() {
        let dir = repo_with(&["app"]);
        // As `git mv app/a.ts app/b.ts` leaves it: renamed on disk, both paths
        // in the index (the old one gone).
        fs::rename(dir.join("app/a.ts"), dir.join("app/b.ts")).unwrap();
        {
            let repo = Repository::open(&dir).unwrap();
            let mut index = repo.index().unwrap();
            index.remove_path(Path::new("app/a.ts")).unwrap();
            index.add_path(Path::new("app/b.ts")).unwrap();
            index.write().unwrap();
        }
        // libgit2 reports it as one staged rename, named by its OLD path, which
        // is no longer on disk: adding that path back fails ("No such file"),
        // so what's staged already is left to the index.
        {
            let repo = Repository::open(&dir).unwrap();
            let statuses = statuses_in(&repo, &["app".to_string()]).unwrap();
            let entries: Vec<(String, Status)> = statuses
                .iter()
                .map(|entry| (entry.path().unwrap_or_default().to_string(), entry.status()))
                .collect();
            assert_eq!(entries.len(), 1, "{entries:?}");
            assert_eq!(entries[0].0, "app/a.ts", "the rename's old path");
            assert!(entries[0].1.contains(Status::INDEX_RENAMED));
            assert!(
                !entries[0]
                    .1
                    .intersects(Status::WT_DELETED | Status::WT_RENAMED),
                "nothing in the working tree is left to stage"
            );
            assert!(
                !dir.join("app/a.ts").exists(),
                "the old path isn't on disk to be added"
            );
        }
        git_commit_in(path(&dir), "rename".into(), false, scopes(&["app"])).unwrap();
        let repo = Repository::open(&dir).unwrap();
        let tree = repo.head().unwrap().peel_to_tree().unwrap();
        assert!(tree.get_path(Path::new("app/b.ts")).is_ok(), "the new name");
        assert!(
            tree.get_path(Path::new("app/a.ts")).is_err(),
            "and not the old one"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_renamed_in_a_terminal_is_committed_under_its_new_name() {
        let dir = repo_with(&["app"]);
        // A plain `mv` in a terminal: gone at the old path, new at the other,
        // nothing staged.
        fs::rename(dir.join("app/a.ts"), dir.join("app/b.ts")).unwrap();
        git_commit_in(path(&dir), "rename".into(), false, scopes(&["app"])).unwrap();
        let repo = Repository::open(&dir).unwrap();
        let tree = repo.head().unwrap().peel_to_tree().unwrap();
        assert!(tree.get_path(Path::new("app/b.ts")).is_ok(), "the new name");
        assert!(
            tree.get_path(Path::new("app/a.ts")).is_err(),
            "and not the old one"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_edit_to_a_file_renamed_in_a_terminal_is_committed() {
        let dir = repo_with(&["app"]);
        // `git mv app/a.ts app/b.ts` in a terminal, then the new file edited.
        // libgit2 reports it as one entry named by the OLD path (which is gone
        // from disk), with the working tree's change on the NEW one.
        fs::rename(dir.join("app/a.ts"), dir.join("app/b.ts")).unwrap();
        {
            let repo = Repository::open(&dir).unwrap();
            let mut index = repo.index().unwrap();
            index.remove_path(Path::new("app/a.ts")).unwrap();
            index.add_path(Path::new("app/b.ts")).unwrap();
            index.write().unwrap();
        }
        fs::write(dir.join("app/b.ts"), "edited after the rename\n").unwrap();
        git_commit_in(
            path(&dir),
            "rename and edit".into(),
            false,
            scopes(&["app"]),
        )
        .unwrap();
        let repo = Repository::open(&dir).unwrap();
        let tree = repo.head().unwrap().peel_to_tree().unwrap();
        let blob = tree
            .get_path(Path::new("app/b.ts"))
            .unwrap()
            .to_object(&repo)
            .unwrap();
        assert_eq!(
            blob.peel_to_blob().unwrap().content(),
            b"edited after the rename\n",
            "the edit made after the rename is in the commit"
        );
        assert!(
            tree.get_path(Path::new("app/a.ts")).is_err(),
            "and the old name isn't"
        );
        // Nothing left over: the working tree is committed.
        let status = git_status_in(path(&dir), scopes(&["app"])).unwrap();
        assert!(changed(&status).is_empty(), "{:?}", changed(&status));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_renamed_in_a_terminal_then_deleted_is_committed_as_gone() {
        let dir = repo_with(&["app"]);
        fs::rename(dir.join("app/a.ts"), dir.join("app/b.ts")).unwrap();
        {
            let repo = Repository::open(&dir).unwrap();
            let mut index = repo.index().unwrap();
            index.remove_path(Path::new("app/a.ts")).unwrap();
            index.add_path(Path::new("app/b.ts")).unwrap();
            index.write().unwrap();
        }
        // Removed after the rename: the commit must have neither name.
        fs::remove_file(dir.join("app/b.ts")).unwrap();
        git_commit_in(
            path(&dir),
            "rename then delete".into(),
            false,
            scopes(&["app"]),
        )
        .unwrap();
        let repo = Repository::open(&dir).unwrap();
        let tree = repo.head().unwrap().peel_to_tree().unwrap();
        assert!(
            tree.get_path(Path::new("app/b.ts")).is_err(),
            "the deleted file isn't committed"
        );
        assert!(tree.get_path(Path::new("app/a.ts")).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_folder_beside_a_nested_repository_is_committed_too() {
        let dir = repo_with(&["app"]);
        // A repository of its own in the folder: its own work is left alone.
        let inner = dir.join("app/inner");
        fs::create_dir_all(&inner).unwrap();
        Repository::init(&inner).unwrap();
        fs::write(inner.join("x.ts"), "x\n").unwrap();
        // A new folder of our own, beside it, with a file in a folder in it.
        fs::create_dir_all(dir.join("app/newdir/deep")).unwrap();
        fs::write(dir.join("app/newdir/a.ts"), "a\n").unwrap();
        fs::write(dir.join("app/newdir/deep/b.ts"), "b\n").unwrap();
        git_commit_in(path(&dir), "new folder".into(), false, scopes(&["app"])).unwrap();
        let repo = Repository::open(&dir).unwrap();
        let tree = repo.head().unwrap().peel_to_tree().unwrap();
        for wanted in ["app/newdir/a.ts", "app/newdir/deep/b.ts"] {
            assert!(
                tree.get_path(Path::new(wanted)).is_ok(),
                "{wanted} is in the commit"
            );
        }
        assert!(
            tree.get_path(Path::new("app/inner/x.ts")).is_err(),
            "the nested repository's file isn't"
        );
        assert!(inner.join("x.ts").exists(), "and it's still on disk");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_repository_deep_inside_a_new_folder_does_not_stop_the_commit() {
        let dir = repo_with(&["app"]);
        // A new folder with our files, and a repository of its own two
        // folders down: every read of the folder falls back, and the folder
        // holding the `.git` is the one left out.
        fs::create_dir_all(dir.join("app/newdir/vendor/lib")).unwrap();
        fs::write(dir.join("app/newdir/a.ts"), "a\n").unwrap();
        Repository::init(dir.join("app/newdir/vendor/lib")).unwrap();
        fs::write(dir.join("app/newdir/vendor/lib/x.ts"), "x\n").unwrap();
        git_commit_in(path(&dir), "new folder".into(), false, scopes(&["app"])).unwrap();
        let repo = Repository::open(&dir).unwrap();
        let tree = repo.head().unwrap().peel_to_tree().unwrap();
        assert!(
            tree.get_path(Path::new("app/newdir/a.ts")).is_ok(),
            "our file is committed"
        );
        assert!(
            tree.get_path(Path::new("app/newdir/vendor/lib/x.ts"))
                .is_err(),
            "the nested repository's file isn't"
        );
        assert!(
            dir.join("app/newdir/vendor/lib/x.ts").exists(),
            "and it's still on disk"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_listed_as_one_row_is_staged_by_reading_the_folder() {
        let dir = repo_with(&["app"]);
        fs::create_dir_all(dir.join("app/newdir/deep")).unwrap();
        fs::write(dir.join("app/newdir/a.ts"), "a\n").unwrap();
        fs::write(dir.join("app/newdir/deep/b.ts"), "b\n").unwrap();
        let repo = Repository::open(&dir).unwrap();
        // The shape a read without descending into untracked folders gives
        // (the fallback `statuses_in` takes): the new folder as one row, with
        // no file of it. Staging that row alone would leave every file out.
        let mut options = status_options_in_scopes(&["app".to_string()]);
        options.recurse_untracked_dirs(false);
        let statuses = repo.statuses(Some(&mut options)).unwrap();
        let rows: Vec<String> = statuses
            .iter()
            .map(|entry| entry.path().unwrap_or_default().to_string())
            .collect();
        assert_eq!(rows, ["app/newdir/"], "one row for the folder");
        let mut index = repo.index().unwrap();
        stage_entries(&mut index, &repo, &statuses, None).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        for wanted in ["app/newdir/a.ts", "app/newdir/deep/b.ts"] {
            assert!(
                tree.get_path(Path::new(wanted)).is_ok(),
                "{wanted} was staged"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_change_staged_outside_the_folder_stops_the_commit_and_stays_staged() {
        let dir = repo_with(&["app", "app2"]);
        fs::write(dir.join("app/a.ts"), "one\n").unwrap();
        fs::write(dir.join("app2/a.ts"), "two\n").unwrap();
        // A terminal's `git add` beside the folder.
        {
            let repo = Repository::open(&dir).unwrap();
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("app2/a.ts")).unwrap();
            index.write().unwrap();
        }
        let err = git_commit_in(path(&dir.join("app")), "wip".into(), false, None).unwrap_err();
        assert!(err.contains("app2/a.ts"), "{err}");
        assert!(err.contains("staged outside"), "{err}");
        // What was staged stays staged, and nothing was committed.
        let status = git_status_in(path(&dir), Some(vec![String::new()])).unwrap();
        let staged: Vec<&str> = status
            .files
            .iter()
            .filter(|file| file.staged)
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(staged, ["app2/a.ts"]);
        let repo = Repository::open(&dir).unwrap();
        assert_eq!(
            repo.head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .summary()
                .unwrap(),
            Some("init"),
            "no commit was made"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_nested_repository_is_left_alone_by_the_status_and_the_commit() {
        let dir = repo_with(&["app"]);
        let inner = dir.join("app/inner");
        fs::create_dir_all(&inner).unwrap();
        Repository::init(&inner).unwrap();
        fs::write(inner.join("x.ts"), "x\n").unwrap();
        fs::write(dir.join("app/a.ts"), "changed\n").unwrap();
        // The outer repository lists its own change only.
        let status = git_status_in(path(&dir), scopes(&["app"])).unwrap();
        assert_eq!(changed(&status), ["app/a.ts"]);
        git_commit_in(path(&dir), "outer".into(), false, scopes(&["app"])).unwrap();
        // The nested repository keeps its own work.
        assert!(inner.join("x.ts").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reverting_a_nested_repository_is_refused() {
        let dir = repo_with(&["app"]);
        let inner = dir.join("app/inner");
        fs::create_dir_all(&inner).unwrap();
        Repository::init(&inner).unwrap();
        fs::write(inner.join("x.ts"), "x\n").unwrap();
        let err = git_revert_file_blocking(path(&dir), "app/inner/x.ts".into()).unwrap_err();
        assert!(err.contains("a repository of its own"), "{err}");
        assert!(inner.join("x.ts").exists(), "nothing was thrown away");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_commit_with_nothing_in_it_is_refused() {
        let dir = repo_with(&["app"]);
        let err = git_commit_in(path(&dir), "empty".into(), false, None).unwrap_err();
        assert_eq!(err, "Nothing to commit.");
        let _ = fs::remove_dir_all(&dir);
    }

    /// An outer repository with a submodule `sub`, as git makes one: the
    /// module's own repository committed once, recorded as a gitlink with a
    /// `.gitmodules` entry, and cloned into place.
    fn repo_with_submodule() -> (PathBuf, PathBuf) {
        let base = temp("submodule");
        // The submodule's repository, at its first commit.
        let sub_dir = base.join("sub");
        fs::create_dir_all(&sub_dir).unwrap();
        fs::write(sub_dir.join("a.ts"), "a\n").unwrap();
        let sub = Repository::init(&sub_dir).unwrap();
        let first = {
            let mut index = sub.index().unwrap();
            index
                .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree = sub.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = git2::Signature::now("t", "t@t").unwrap();
            sub.commit(Some("HEAD"), &sig, &sig, "sub", &tree, &[])
                .unwrap()
        };
        drop(sub);
        // The outer repository: the module's commit as a gitlink, and the
        // `.gitmodules` entry git writes with it.
        let outer = base.join("outer");
        fs::create_dir_all(&outer).unwrap();
        let repo = Repository::init(&outer).unwrap();
        fs::write(outer.join("top.ts"), "t\n").unwrap();
        fs::write(
            outer.join(".gitmodules"),
            "[submodule \"sub\"]\n\tpath = sub\n\turl = ./sub\n",
        )
        .unwrap();
        {
            let mut index = repo.index().unwrap();
            let gitlink = git2::IndexEntry {
                ctime: git2::IndexTime::new(0, 0),
                mtime: git2::IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: 0o160000,
                uid: 0,
                gid: 0,
                file_size: 0,
                id: first,
                flags: 0,
                flags_extended: 0,
                path: b"sub".to_vec(),
            };
            index.add(&gitlink).unwrap();
            index.add_path(Path::new("top.ts")).unwrap();
            index.add_path(Path::new(".gitmodules")).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = git2::Signature::now("t", "t@t").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "outer", &tree, &[])
                .unwrap();
        }
        // The module checked out, at the commit recorded for it.
        let sub_dir = outer.join("sub");
        Repository::clone(&base.join("sub").to_string_lossy(), &sub_dir).unwrap();
        (outer, sub_dir)
    }

    /// One commit of everything in `dir`: the submodule's own history moving.
    fn commit_in(dir: &std::path::Path, message: &str) {
        let repo = Repository::open(dir).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&head])
            .unwrap();
    }

    #[test]
    fn a_submodule_moved_to_a_new_commit_is_listed_and_committed() {
        let (outer, sub) = repo_with_submodule();
        // An edit inside the module: the module's own business, not a change
        // of this repository's.
        fs::write(sub.join("a.ts"), "a edited\n").unwrap();
        let status = git_status_in(path(&outer), None).unwrap();
        assert!(changed(&status).is_empty(), "{:?}", changed(&status));
        // The module moves to a newer commit (`cd sub && git pull`). Both
        // arrive as `WT_MODIFIED`; this one records the module's commit, so
        // it's this repository's change, as `git add` makes it.
        commit_in(&sub, "the module moves on");
        let status = git_status_in(path(&outer), None).unwrap();
        assert_eq!(changed(&status), ["sub"], "the module's new commit shows");
        git_commit_in(path(&outer), "bump the module".into(), false, None).unwrap();
        let repo = Repository::open(&outer).unwrap();
        let tree = repo.head().unwrap().peel_to_tree().unwrap();
        let recorded = tree.get_path(Path::new("sub")).unwrap().id();
        let checked_out = Repository::open(&sub)
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap();
        assert_eq!(
            recorded, checked_out,
            "the commit it has is the one recorded"
        );
        assert!(
            !sub.join("a edited").exists(),
            "the module's own files aren't the outer repository's"
        );
        let _ = fs::remove_dir_all(outer.parent().unwrap());
    }

    #[test]
    fn a_submodule_with_only_edits_inside_is_left_alone_by_a_commit() {
        let (outer, sub) = repo_with_submodule();
        fs::write(sub.join("a.ts"), "a edited\n").unwrap();
        // Something of our own to commit, beside it.
        fs::write(outer.join("top.ts"), "changed\n").unwrap();
        git_commit_in(path(&outer), "our change".into(), false, None).unwrap();
        let repo = Repository::open(&outer).unwrap();
        let tree = repo.head().unwrap().peel_to_tree().unwrap();
        let recorded = tree.get_path(Path::new("sub")).unwrap().id();
        let index = repo.index().unwrap();
        assert_eq!(
            index.get_path(Path::new("sub"), 0).map(|e| e.id),
            Some(recorded),
            "the module's commit is unchanged"
        );
        assert!(
            sub.join("a.ts").exists(),
            "the module's edit is still there, uncommitted"
        );
        let _ = fs::remove_dir_all(outer.parent().unwrap());
    }

    #[test]
    fn a_submodule_is_another_repositorys_business() {
        let dir = repo_with(&["app"]);
        let repo = Repository::open(&dir).unwrap();
        assert!(!is_submodule(&repo, Path::new("app/a.ts")));
        // A gitlink, as git records a submodule's commit, is a submodule.
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let entry = git2::IndexEntry {
            ctime: git2::IndexTime::new(0, 0),
            mtime: git2::IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            mode: 0o160000,
            uid: 0,
            gid: 0,
            file_size: 0,
            id: head.id(),
            flags: 0,
            flags_extended: 0,
            path: b"app/sub".to_vec(),
        };
        {
            let mut index = repo.index().unwrap();
            index.add(&entry).unwrap();
            index.write().unwrap();
        }
        assert!(is_submodule(&repo, Path::new("app/sub")));
        assert!(
            is_submodule(&repo, Path::new("app/sub/")),
            "a folder's entry"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}

/// A commit across the window's repositories (D11): every check first, then
/// every hook, then the commits, each on the branch its repository is on.
mod commit_many_tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "git-commit-many-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn p(dir: &Path) -> String {
        dir.to_string_lossy().into_owned()
    }

    /// Who commits, and where the hooks are, whatever the machine's global
    /// config says.
    fn configure(repo: &Repository) {
        let mut c = repo.config().unwrap();
        c.set_str("user.name", "t").unwrap();
        c.set_str("user.email", "t@t").unwrap();
        c.set_str("core.hooksPath", ".git/hooks").unwrap();
    }

    /// A repository on `main` with one commit, and `a.txt` changed since.
    fn repo(tag: &str) -> (PathBuf, Repository) {
        let dir = temp_dir(tag);
        let repo = Repository::init(&dir).unwrap();
        configure(&repo);
        fs::write(dir.join("a.txt"), "one\n").unwrap();
        git_commit_blocking(p(&dir), "start".into()).unwrap();
        if repo.head().unwrap().shorthand().ok() != Some("main") {
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            repo.branch("main", &head, true).unwrap();
            repo.set_head("refs/heads/main").unwrap();
        }
        fs::write(dir.join("a.txt"), "two\n").unwrap();
        (dir, repo)
    }

    /// Each repository whole: a workspace whose folders are repositories.
    fn whole(dirs: &[&Path]) -> Vec<RepoCommit> {
        dirs.iter()
            .map(|dir| RepoCommit {
                path: p(dir),
                scopes: vec![String::new()],
            })
            .collect()
    }

    fn head_id(repo: &Repository) -> git2::Oid {
        head_commit(repo).unwrap().id()
    }

    fn head_message(repo: &Repository) -> String {
        head_commit(repo).unwrap().message().unwrap().to_string()
    }

    fn tip(repo: &Repository, branch: &str) -> git2::Oid {
        repo.find_branch(branch, BranchType::Local)
            .unwrap()
            .get()
            .target()
            .unwrap()
    }

    /// Hook `name` of the repository at `dir`, a shell script running `body`.
    #[cfg(unix)]
    fn hook(dir: &Path, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let folder = dir.join(".git/hooks");
        fs::create_dir_all(&folder).unwrap();
        let file = folder.join(name);
        fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_pre_commit_failing_in_the_second_repository_leaves_the_first_uncommitted() {
        let (api, api_repo) = repo("pre-commit-api");
        let (web, web_repo) = repo("pre-commit-web");
        let (api_before, web_before) = (head_id(&api_repo), head_id(&web_repo));
        hook(&web, "pre-commit", "echo 'lint: 2 errors' >&2\nexit 1");
        let outcome = git_commit_many_in(whole(&[&api, &web]), "change".into(), false).unwrap();
        assert_eq!(
            outcome,
            CommitOutcome::Stopped {
                problems: vec![RepoProblem {
                    path: p(&web),
                    error:
                        "Not committed: the pre-commit hook failed (exit code 1):\nlint: 2 errors"
                            .into(),
                }],
            }
        );
        assert_eq!(
            head_id(&api_repo),
            api_before,
            "the first repository's commit waited for every hook"
        );
        assert_eq!(head_id(&web_repo), web_before);
    }

    #[cfg(unix)]
    #[test]
    fn without_hooks_no_check_runs_in_any_repository() {
        let (api, api_repo) = repo("no-verify-api");
        let (web, web_repo) = repo("no-verify-web");
        for dir in [&api, &web] {
            hook(dir, "pre-commit", "echo pre-commit >> .git/ran\nexit 1");
            hook(dir, "commit-msg", "echo commit-msg >> .git/ran\nexit 1");
        }
        let outcome = git_commit_many_in(whole(&[&api, &web]), "wip".into(), true).unwrap();
        assert_eq!(
            outcome,
            CommitOutcome::Done {
                committed: vec![p(&api), p(&web)],
                failed: vec![],
            }
        );
        for (dir, repo) in [(&api, &api_repo), (&web, &web_repo)] {
            assert_eq!(head_message(repo), "wip");
            assert!(
                !dir.join(".git/ran").exists(),
                "no check ran in {}",
                dir.display()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_commit_msg_hook_changes_its_own_repositorys_message_only() {
        let (api, api_repo) = repo("message-api");
        let (web, web_repo) = repo("message-web");
        hook(
            &api,
            "commit-msg",
            "printf '[API] %s' \"$(cat \"$1\")\" > \"$1\"",
        );
        git_commit_many_in(whole(&[&api, &web]), "fix it".into(), false).unwrap();
        assert_eq!(head_message(&api_repo), "[API] fix it\n");
        assert_eq!(head_message(&web_repo), "fix it");
    }

    #[cfg(unix)]
    #[test]
    fn a_commit_is_dated_when_it_is_made_after_its_hooks() {
        let (api, api_repo) = repo("dated-api");
        // A slow pre-commit, which notes when it finished.
        hook(&api, "pre-commit", "sleep 1\ndate +%s > .git/hooked");
        git_commit_many_in(whole(&[&api]), "change".into(), false).unwrap();
        let hooked: i64 = fs::read_to_string(api.join(".git/hooked"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let head = head_commit(&api_repo).unwrap();
        for (who, when) in [
            ("author", head.author().when()),
            ("committer", head.committer().when()),
        ] {
            assert!(
                when.seconds() >= hooked,
                "the {who} is dated {}, before its hook finished at {hooked}",
                when.seconds()
            );
        }
    }

    #[test]
    fn each_commit_goes_on_the_branch_its_repository_is_on() {
        let (api, api_repo) = repo("branch-api");
        let (web, web_repo) = repo("branch-web");
        // api works on a branch of its own; web stays on main.
        let start = head_id(&api_repo);
        let at = api_repo.find_commit(start).unwrap();
        api_repo.branch("feature", &at, false).unwrap();
        api_repo.set_head("refs/heads/feature").unwrap();
        git_commit_many_in(whole(&[&api, &web]), "change".into(), false).unwrap();
        assert_eq!(tip(&api_repo, "main"), start, "api's main stayed");
        let feature = api_repo.find_commit(tip(&api_repo, "feature")).unwrap();
        assert_eq!(feature.parent_id(0).unwrap(), start, "api's is on feature");
        assert_eq!(head_id(&web_repo), tip(&web_repo, "main"), "web's on main");
        assert_eq!(head_message(&web_repo), "change");
    }

    #[test]
    fn a_detached_head_gets_its_commit() {
        let (api, api_repo) = repo("detached-api");
        let (web, _web_repo) = repo("detached-web");
        let start = head_id(&api_repo);
        api_repo.set_head_detached(start).unwrap();
        git_commit_many_in(whole(&[&api, &web]), "change".into(), false).unwrap();
        assert!(api_repo.head_detached().unwrap(), "as git leaves it");
        let head = head_commit(&api_repo).unwrap();
        assert_eq!(head.parent_id(0).unwrap(), start);
        assert_eq!(tip(&api_repo, "main"), start, "no branch moved");
    }

    #[test]
    fn a_repository_with_no_commit_gets_its_branch() {
        let (api, _api_repo) = repo("unborn-api");
        let web = temp_dir("unborn-web");
        let web_repo = Repository::init(&web).unwrap();
        configure(&web_repo);
        fs::write(web.join("b.txt"), "b\n").unwrap();
        // The branch HEAD names before it has a commit (init.defaultBranch's).
        let branch = web_repo
            .find_reference("HEAD")
            .unwrap()
            .symbolic_target()
            .unwrap()
            .unwrap()
            .to_string();
        git_commit_many_in(whole(&[&api, &web]), "first".into(), false).unwrap();
        let head = web_repo.head().unwrap();
        assert_eq!(head.name().unwrap(), branch, "the commit made it");
        assert_eq!(head.peel_to_commit().unwrap().parent_count(), 0);
    }

    #[test]
    fn a_commit_that_fails_after_others_were_made_names_them() {
        let (api, api_repo) = repo("lock-api");
        let (web, web_repo) = repo("lock-web");
        let (infra, infra_repo) = repo("lock-infra");
        let before = [head_id(&api_repo), head_id(&web_repo), head_id(&infra_repo)];
        // A terminal's git holds infra's branch.
        fs::write(infra.join(".git/refs/heads/main.lock"), "").unwrap();
        let outcome =
            git_commit_many_in(whole(&[&api, &web, &infra]), "change".into(), false).unwrap();
        let CommitOutcome::Done { committed, failed } = outcome else {
            panic!("the first two are committed: {outcome:?}");
        };
        assert_eq!(committed, [p(&api), p(&web)]);
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert_eq!(failed[0].path, p(&infra));
        assert!(failed[0].error.contains("lock"), "{}", failed[0].error);
        assert_ne!(head_id(&api_repo), before[0]);
        assert_ne!(head_id(&web_repo), before[1]);
        assert_eq!(head_id(&infra_repo), before[2], "infra wasn't committed");
    }

    #[test]
    fn every_problem_a_check_finds_is_named_and_nothing_is_committed() {
        let (api, api_repo) = repo("checks-api");
        let (web, web_repo) = repo("checks-web");
        let (docs, docs_repo) = repo("checks-docs");
        // api is in the middle of a merge.
        fs::write(
            api.join(".git/MERGE_HEAD"),
            format!("{}\n", head_id(&api_repo)),
        )
        .unwrap();
        // The window has web's `app` folder, and a terminal staged a change
        // beside it.
        fs::create_dir_all(web.join("app")).unwrap();
        fs::write(web.join("app/x.txt"), "x\n").unwrap();
        {
            let mut index = web_repo.index().unwrap();
            index.add_path(Path::new("a.txt")).unwrap();
            index.write().unwrap();
        }
        let docs_before = head_id(&docs_repo);
        let repos = vec![
            RepoCommit {
                path: p(&api),
                scopes: vec![String::new()],
            },
            RepoCommit {
                path: p(&web.join("app")),
                scopes: vec!["app".into()],
            },
            RepoCommit {
                path: p(&docs),
                scopes: vec![String::new()],
            },
        ];
        let outcome = git_commit_many_in(repos, "change".into(), false).unwrap();
        assert_eq!(
            outcome,
            CommitOutcome::Stopped {
                problems: vec![
                    RepoProblem {
                        path: p(&api),
                        error: "A merge is in progress: finish or abort it first.".into(),
                    },
                    RepoProblem {
                        path: p(&web.join("app")),
                        error: "Changes are staged outside this window's folders: a.txt. Commit or unstage them in a terminal first.".into(),
                    },
                ],
            }
        );
        assert_eq!(head_id(&docs_repo), docs_before, "nothing anywhere");
        assert!(
            web_repo
                .status_file(Path::new("a.txt"))
                .unwrap()
                .is_index_modified(),
            "what was staged stays staged"
        );
    }

    #[test]
    fn nothing_to_commit_in_any_repository_is_said_so() {
        let (api, _api_repo) = repo("nothing-api");
        fs::write(api.join("a.txt"), "one\n").unwrap();
        let err = git_commit_many_in(whole(&[&api]), "empty".into(), false).unwrap_err();
        assert_eq!(err, "Nothing to commit.");
    }
}

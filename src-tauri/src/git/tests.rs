use super::branches::validate_branch_name;
use super::history::{compact_contains, git_log_blocking, git_log_scan};
use super::remote::credential_fill;
use super::*;
use git2::{BranchType, Repository};

mod revert_tests {
    use super::*;
    use std::fs;

    fn repo_with_commit() -> (PathBuf, Repository) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "git-revert-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
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
        let b = st.files.iter().find(|f| f.path == "b.txt").unwrap();
        assert!(b.unstaged && !b.staged);
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

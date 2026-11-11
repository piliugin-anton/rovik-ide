use super::*;
use rovik_core::merge::{parse_merged, MARKER};
use std::fs;
use std::path::PathBuf;

fn temp_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "git-sync-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn commit_all(repo: &Repository, message: &str) -> Oid {
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

fn checkout(repo: &Repository, branch: &str) {
    let refname = format!("refs/heads/{branch}");
    let obj = repo.revparse_single(&refname).unwrap();
    repo.checkout_tree(&obj, Some(CheckoutBuilder::new().force()))
        .unwrap();
    repo.set_head(&refname).unwrap();
}

/// `main`, tracking a local branch `up` (remote "."), both one commit past
/// a shared start. `ours` / `theirs` are a.txt's contents on each.
fn diverged(ours: &str, theirs: &str) -> (PathBuf, Repository) {
    let dir = temp_dir("repo");
    let repo = Repository::init(&dir).unwrap();
    {
        let mut c = repo.config().unwrap();
        c.set_str("user.name", "t").unwrap();
        c.set_str("user.email", "t@t").unwrap();
    }
    fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    fs::write(dir.join("b.txt"), "b\n").unwrap();
    commit_all(&repo, "start");
    let main = repo.head().unwrap().shorthand().unwrap().to_string();
    if main != "main" {
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("main", &head, true).unwrap();
        checkout(&repo, "main");
    }
    {
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("up", &head, false).unwrap();
    }
    checkout(&repo, "up");
    fs::write(dir.join("a.txt"), theirs).unwrap();
    commit_all(&repo, "theirs");
    checkout(&repo, "main");
    fs::write(dir.join("a.txt"), ours).unwrap();
    commit_all(&repo, "ours");
    {
        let mut c = repo.config().unwrap();
        c.set_str("branch.main.remote", ".").unwrap();
        c.set_str("branch.main.merge", "refs/heads/up").unwrap();
    }
    (dir, repo)
}

fn p(dir: &Path) -> String {
    dir.to_string_lossy().into_owned()
}

#[test]
fn parse_merged_splits_common_text_and_conflicts() {
    let m = |c: char, label: &str| format!("{}{label}\n", c.to_string().repeat(MARKER));
    let text = format!(
        "a\n{}x\n{}o\n{}y\n{}b\n",
        m('<', " ours"),
        m('|', " base"),
        m('=', ""),
        m('>', " theirs")
    );
    assert_eq!(
        parse_merged(&text),
        vec![
            MergeSegment::Common { text: "a\n".into() },
            MergeSegment::Conflict {
                ours: "x\n".into(),
                base: "o\n".into(),
                theirs: "y\n".into()
            },
            MergeSegment::Common { text: "b\n".into() },
        ]
    );
    // Markers of git's usual length are file content here.
    assert_eq!(
        parse_merged("<<<<<<< x\n=======\n"),
        vec![MergeSegment::Common {
            text: "<<<<<<< x\n=======\n".into()
        }]
    );
}

#[test]
fn pull_config_follows_gits_precedence() {
    let (_dir, repo) = diverged("x\n", "y\n");
    let mut c = repo
        .config()
        .unwrap()
        .open_level(git2::ConfigLevel::Local)
        .unwrap();
    // Whatever the machine's global config says, the local one decides here.
    c.set_bool("pull.rebase", false).unwrap();
    c.set_str("pull.ff", "true").unwrap();
    let cfg = pull_config(&repo, "main");
    assert_eq!(cfg.strategy, Some(PullStrategy::Merge));
    assert_eq!(cfg.source.as_deref(), Some("pull.rebase"));

    c.set_str("pull.rebase", "merges").unwrap();
    assert_eq!(
        pull_config(&repo, "main").strategy,
        Some(PullStrategy::Rebase)
    );

    c.set_str("branch.main.rebase", "false").unwrap();
    let cfg = pull_config(&repo, "main");
    assert_eq!(cfg.strategy, Some(PullStrategy::Merge));
    assert_eq!(cfg.source.as_deref(), Some("branch.main.rebase"));

    c.set_str("pull.ff", "only").unwrap();
    assert_eq!(
        pull_config(&repo, "main").strategy,
        Some(PullStrategy::FfOnly)
    );

    c.set_str("pull.ff", "false").unwrap();
    assert!(pull_config(&repo, "main").no_ff);
}

#[test]
fn diverged_without_a_strategy_asks() {
    let (_dir, repo) = diverged("one\ntwo\nthree\nours\n", "zero\none\ntwo\nthree\n");
    let mut c = repo
        .config()
        .unwrap()
        .open_level(git2::ConfigLevel::Local)
        .unwrap();
    // A strategy from the machine's global config would answer; blank it out.
    c.set_str("pull.ff", "true").unwrap();
    if pull_config(&repo, "main").strategy.is_some() {
        return; // the global config decides on this machine
    }
    let out = integrate_upstream(&repo, None, false).unwrap();
    assert_eq!(out.result, "needs-strategy");
    assert_eq!(repo.state(), RepositoryState::Clean);
}

#[test]
fn merge_without_conflicts_commits_with_two_parents() {
    let (dir, repo) = diverged("one\ntwo\nthree\nours\n", "zero\none\ntwo\nthree\n");
    let out = integrate_upstream(&repo, Some(PullStrategy::Merge), false).unwrap();
    assert_eq!(out.result, "merged");
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.parent_count(), 2);
    assert_eq!(
        fs::read_to_string(dir.join("a.txt")).unwrap(),
        "zero\none\ntwo\nthree\nours\n"
    );
    assert_eq!(repo.state(), RepositoryState::Clean);
}

#[test]
fn merge_conflict_resolve_and_continue() {
    let (dir, repo) = diverged("one\nOURS\nthree\n", "one\nTHEIRS\nthree\n");
    let out = integrate_upstream(&repo, Some(PullStrategy::Merge), false).unwrap();
    assert_eq!(out.result, "conflicts");
    assert_eq!(out.conflicts, vec!["a.txt".to_string()]);

    let op = current_operation(&repo).unwrap();
    assert_eq!(op.kind, "merge");
    assert_eq!(op.conflicts, vec!["a.txt".to_string()]);
    assert!(op.can_continue);

    let file = git_conflict_file_blocking(p(&dir), "a.txt".into()).unwrap();
    assert!(!file.resolved && !file.binary);
    assert_eq!(
        file.segments,
        vec![
            MergeSegment::Common {
                text: "one\n".into()
            },
            // As git's markers in the file: the two sides only.
            MergeSegment::Conflict {
                ours: "OURS\n".into(),
                base: String::new(),
                theirs: "THEIRS\n".into()
            },
            MergeSegment::Common {
                text: "three\n".into()
            },
        ]
    );

    // Continuing before resolving is refused.
    assert!(git_operation_continue_blocking(p(&dir), None).is_err());

    git_conflict_resolve_blocking(
        p(&dir),
        "a.txt".into(),
        Some("one\nOURS\nTHEIRS\nthree\n".into()),
        None,
    )
    .unwrap();
    assert!(conflicted_paths(&repo).unwrap().is_empty());

    // A resolved file can be opened again, its sides from the commits.
    let again = git_conflict_file_blocking(p(&dir), "a.txt".into()).unwrap();
    assert!(again.resolved);
    assert_eq!(again.ours.as_deref(), Some("one\nOURS\nthree\n"));
    assert_eq!(again.theirs.as_deref(), Some("one\nTHEIRS\nthree\n"));
    assert_eq!(again.current.as_deref(), Some("one\nOURS\nTHEIRS\nthree\n"));

    let out = git_operation_continue_blocking(p(&dir), None).unwrap();
    assert_eq!(out.result, "merged");
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.parent_count(), 2);
    assert!(head.message().unwrap().starts_with("Merge"));
    assert_eq!(repo.state(), RepositoryState::Clean);
}

#[test]
fn rebase_conflict_resolve_and_continue() {
    let (dir, repo) = diverged("one\nOURS\nthree\n", "one\nTHEIRS\nthree\n");
    let up = repo.revparse_single("refs/heads/up").unwrap().id();
    let out = integrate_upstream(&repo, Some(PullStrategy::Rebase), false).unwrap();
    assert_eq!(out.result, "conflicts");

    let op = current_operation(&repo).unwrap();
    assert_eq!(op.kind, "rebase");
    assert_eq!(op.branch.as_deref(), Some("main"));
    assert_eq!((op.step, op.total), (Some(1), Some(1)));
    assert_eq!(op.commit.as_deref(), Some("ours"));

    let file = git_conflict_file_blocking(p(&dir), "a.txt".into()).unwrap();
    // Rebasing, "ours" is the upstream and "theirs" the commit replayed.
    assert!(matches!(
        &file.segments[1],
        MergeSegment::Conflict { ours, theirs, .. } if ours == "THEIRS\n" && theirs == "OURS\n"
    ));

    git_conflict_resolve_blocking(
        p(&dir),
        "a.txt".into(),
        Some("one\nBOTH\nthree\n".into()),
        None,
    )
    .unwrap();
    let out = git_operation_continue_blocking(p(&dir), None).unwrap();
    assert_eq!(out.result, "rebased");
    assert_eq!(repo.state(), RepositoryState::Clean);
    let head = repo.head().unwrap();
    assert_eq!(head.shorthand().ok(), Some("main"));
    let commit = head.peel_to_commit().unwrap();
    assert_eq!(commit.parent_count(), 1);
    assert_eq!(commit.parent_id(0).unwrap(), up);
    assert_eq!(commit.summary().unwrap(), Some("ours"));
    assert_eq!(
        fs::read_to_string(dir.join("a.txt")).unwrap(),
        "one\nBOTH\nthree\n"
    );
}

#[test]
fn rebase_without_conflicts_is_linear() {
    let (dir, repo) = diverged("one\ntwo\nthree\nours\n", "zero\none\ntwo\nthree\n");
    let up = repo.revparse_single("refs/heads/up").unwrap().id();
    let out = integrate_upstream(&repo, Some(PullStrategy::Rebase), false).unwrap();
    assert_eq!(out.result, "rebased");
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(commit.parent_id(0).unwrap(), up);
    assert_eq!(
        fs::read_to_string(dir.join("a.txt")).unwrap(),
        "zero\none\ntwo\nthree\nours\n"
    );
}

#[test]
fn abort_puts_the_branch_back() {
    for strategy in [PullStrategy::Merge, PullStrategy::Rebase] {
        let (dir, repo) = diverged("one\nOURS\nthree\n", "one\nTHEIRS\nthree\n");
        let before = repo.head().unwrap().target().unwrap();
        assert_eq!(
            integrate_upstream(&repo, Some(strategy), false)
                .unwrap()
                .result,
            "conflicts"
        );
        git_operation_abort_blocking(p(&dir)).unwrap();
        assert_eq!(repo.state(), RepositoryState::Clean, "{strategy:?}");
        let head = repo.head().unwrap();
        assert_eq!(head.shorthand().ok(), Some("main"));
        assert_eq!(head.target().unwrap(), before);
        assert_eq!(
            fs::read_to_string(dir.join("a.txt")).unwrap(),
            "one\nOURS\nthree\n"
        );
    }
}

/// Aborting a merge puts back what the merge touched and nothing else: an
/// edit not added, to a file the merge didn't touch, stays, as with git's
/// own --abort.
#[test]
fn an_abort_keeps_edits_the_merge_didnt_touch() {
    let (dir, repo) = diverged("one\nOURS\nthree\n", "one\nTHEIRS\nthree\n");
    assert_eq!(
        integrate_upstream(&repo, Some(PullStrategy::Merge), false)
            .unwrap()
            .result,
        "conflicts"
    );
    fs::write(dir.join("b.txt"), "edited meanwhile\n").unwrap();
    git_operation_abort_blocking(p(&dir)).unwrap();
    assert_eq!(repo.state(), RepositoryState::Clean);
    assert_eq!(
        fs::read_to_string(dir.join("a.txt")).unwrap(),
        "one\nOURS\nthree\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("b.txt")).unwrap(),
        "edited meanwhile\n"
    );
}

#[test]
fn remember_stores_pull_rebase() {
    let (_dir, repo) = diverged("one\ntwo\nthree\nours\n", "zero\none\ntwo\nthree\n");
    integrate_upstream(&repo, Some(PullStrategy::Rebase), true).unwrap();
    let local = repo
        .config()
        .unwrap()
        .open_level(git2::ConfigLevel::Local)
        .unwrap();
    assert!(local.get_bool("pull.rebase").unwrap());
}

#[test]
fn ff_only_refuses_diverged_branches() {
    let (_dir, repo) = diverged("x\n", "y\n");
    let err = integrate_upstream(&repo, Some(PullStrategy::FfOnly), false).unwrap_err();
    assert!(err.contains("fast-forward"), "{err}");
}

#[test]
fn uncommitted_changes_stop_a_merge() {
    let (dir, repo) = diverged("one\nOURS\nthree\n", "one\nTHEIRS\nthree\n");
    fs::write(dir.join("b.txt"), "edited\n").unwrap();
    assert!(integrate_upstream(&repo, Some(PullStrategy::Merge), false).is_err());
    assert_eq!(repo.state(), RepositoryState::Clean);
    assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "edited\n");
}

#[test]
fn delete_modify_conflict_resolves_to_a_deletion() {
    let (dir, repo) = diverged("one\nOURS\nthree\n", "x\n");
    // Upstream deletes a.txt instead.
    checkout(&repo, "up");
    fs::remove_file(dir.join("a.txt")).unwrap();
    commit_all(&repo, "delete a");
    checkout(&repo, "main");
    let out = integrate_upstream(&repo, Some(PullStrategy::Merge), false).unwrap();
    assert_eq!(out.result, "conflicts");
    let file = git_conflict_file_blocking(p(&dir), "a.txt".into()).unwrap();
    assert!(file.ours.is_some());
    assert!(file.theirs.is_none());
    git_conflict_resolve_blocking(p(&dir), "a.txt".into(), None, None).unwrap();
    assert_eq!(
        git_operation_continue_blocking(p(&dir), None)
            .unwrap()
            .result,
        "merged"
    );
    assert!(!dir.join("a.txt").exists());
}

#[test]
fn taking_a_whole_side_writes_its_bytes() {
    let (dir, repo) = diverged("one\nOURS\nthree\n", "one\nTHEIRS\nthree\n");
    integrate_upstream(&repo, Some(PullStrategy::Merge), false).unwrap();
    git_conflict_resolve_blocking(p(&dir), "a.txt".into(), None, Some("theirs")).unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("a.txt")).unwrap(),
        "one\nTHEIRS\nthree\n"
    );
    // Chosen again after it was resolved: the sides come from the commits.
    git_conflict_resolve_blocking(p(&dir), "a.txt".into(), None, Some("ours")).unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("a.txt")).unwrap(),
        "one\nOURS\nthree\n"
    );
    assert!(conflicted_paths(&repo).unwrap().is_empty());
}

#[test]
fn a_push_behind_the_remote_is_rejected_not_failed() {
    let remote = temp_dir("remote");
    Repository::init_bare(&remote).unwrap();
    let url = p(&remote);
    let clone = |tag: &str| {
        let dir = temp_dir(tag);
        let repo = Repository::init(&dir).unwrap();
        let mut c = repo.config().unwrap();
        c.set_str("user.name", "t").unwrap();
        c.set_str("user.email", "t@t").unwrap();
        repo.remote("origin", &url).unwrap();
        (dir, repo)
    };
    let (a_dir, a) = clone("a");
    fs::write(a_dir.join("f.txt"), "a\n").unwrap();
    commit_all(&a, "a");
    let branch = a.head().unwrap().shorthand().unwrap().to_string();
    assert_eq!(
        crate::git::git_push_blocking(p(&a_dir)).unwrap().result,
        "pushed"
    );

    let (b_dir, b) = clone("b");
    fs::write(b_dir.join("f.txt"), "b\n").unwrap();
    commit_all(&b, "b");
    if b.head().unwrap().shorthand().ok() != Some(branch.as_str()) {
        let head = b.head().unwrap().peel_to_commit().unwrap();
        b.branch(&branch, &head, true).unwrap();
        b.set_head(&format!("refs/heads/{branch}")).unwrap();
    }
    let out = crate::git::git_push_blocking(p(&b_dir)).unwrap();
    assert_eq!(out.result, "rejected");
    // Rejected means untouched: the remote still has a's commit, not b's.
    let bare = Repository::open_bare(&remote).unwrap();
    let tip = bare.refname_to_id(&format!("refs/heads/{branch}")).unwrap();
    assert_eq!(tip, a.head().unwrap().target().unwrap());
}

/// The push is never a force push: a branch whose pushed commits were
/// rewritten (amend, reset, rebase), with the old tip still known locally,
/// is turned down instead of replacing what the remote has.
#[test]
fn a_push_never_rewrites_the_remote_history() {
    let remote = temp_dir("remote-rewrite");
    Repository::init_bare(&remote).unwrap();
    let dir = temp_dir("rewrite");
    let repo = Repository::init(&dir).unwrap();
    {
        let mut c = repo.config().unwrap();
        c.set_str("user.name", "t").unwrap();
        c.set_str("user.email", "t@t").unwrap();
    }
    repo.remote("origin", &p(&remote)).unwrap();
    fs::write(dir.join("f.txt"), "one\n").unwrap();
    let first = commit_all(&repo, "one");
    fs::write(dir.join("f.txt"), "two\n").unwrap();
    let pushed = commit_all(&repo, "two");
    assert_eq!(
        crate::git::git_push_blocking(p(&dir)).unwrap().result,
        "pushed"
    );
    let branch = repo.head().unwrap().shorthand().unwrap().to_string();
    let remote_tip = || {
        Repository::open_bare(&remote)
            .unwrap()
            .refname_to_id(&format!("refs/heads/{branch}"))
            .unwrap()
    };

    // Amend the pushed commit: same parent, different content.
    fs::write(dir.join("f.txt"), "rewritten\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("f.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = repo.signature().unwrap();
    let parent = repo.find_commit(first).unwrap();
    let amended = repo
        .commit(None, &sig, &sig, "two, amended", &tree, &[&parent])
        .unwrap();
    repo.find_reference(&format!("refs/heads/{branch}"))
        .unwrap()
        .set_target(amended, "amend")
        .unwrap();
    assert_eq!(
        crate::git::git_push_blocking(p(&dir)).unwrap().result,
        "rejected"
    );
    assert_eq!(remote_tip(), pushed);

    // Reset behind the remote: pushing would drop its newest commit.
    repo.find_reference(&format!("refs/heads/{branch}"))
        .unwrap()
        .set_target(first, "reset")
        .unwrap();
    assert_eq!(
        crate::git::git_push_blocking(p(&dir)).unwrap().result,
        "rejected"
    );
    assert_eq!(remote_tip(), pushed);
}

#[cfg(unix)]
#[test]
fn a_mode_change_alone_adds_no_lines() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("mode-only");
    let repo = Repository::init(&dir).unwrap();
    {
        let mut c = repo.config().unwrap();
        c.set_str("user.name", "t").unwrap();
        c.set_str("user.email", "t@t").unwrap();
    }
    fs::write(dir.join("run.sh"), "echo 1\necho 2\n").unwrap();
    commit_all(&repo, "start");
    fs::set_permissions(dir.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(dir.join("new.txt"), "a\nb\nc\n").unwrap();
    let status = crate::git::git_status_blocking(dir.to_string_lossy().into_owned()).unwrap();
    let counts = |name: &str| {
        let f = status.files.iter().find(|f| f.path == name).unwrap();
        (f.additions, f.deletions)
    };
    assert_eq!(counts("run.sh"), (0, 0), "only its mode changed");
    assert_eq!(counts("new.txt"), (3, 0), "a new file still gets its lines");
}

// ---------------------------------------------------------------------------
// Rebase onto a branch (the Git menu's Rebase…)
// ---------------------------------------------------------------------------

/// A local branch `name` at `at`.
fn branch_at(repo: &Repository, name: &str, at: Oid) {
    repo.branch(name, &repo.find_commit(at).unwrap(), true)
        .unwrap();
}

#[test]
fn rebasing_onto_a_branch_replays_the_branchs_commits_on_it() {
    // main ("ours") onto release, which has up's commit but isn't main's
    // upstream.
    let (dir, repo) = diverged("one\ntwo\nthree\nours\n", "zero\none\ntwo\nthree\n");
    let up = repo.revparse_single("refs/heads/up").unwrap().id();
    branch_at(&repo, "release", up);
    let out = git_rebase_blocking(p(&dir), "release".into(), false, false).unwrap();
    assert_eq!(out.result, "rebased");
    let head = repo.head().unwrap();
    assert_eq!(head.shorthand().ok(), Some("main"));
    let commit = head.peel_to_commit().unwrap();
    assert_eq!(commit.summary().unwrap(), Some("ours"));
    assert_eq!(commit.parent_id(0).unwrap(), up);
    assert_eq!(
        fs::read_to_string(dir.join("a.txt")).unwrap(),
        "zero\none\ntwo\nthree\nours\n"
    );
    // main has release's commits now: nothing to do, nothing rewritten.
    let again = git_rebase_blocking(p(&dir), "release".into(), false, false).unwrap();
    assert_eq!(again.result, "up-to-date");
    assert_eq!(repo.head().unwrap().target().unwrap(), commit.id());
}

#[test]
fn a_branch_with_nothing_of_its_own_moves_up_to_the_branch() {
    let (dir, repo) = diverged("one\ntwo\nthree\nours\n", "zero\none\ntwo\nthree\n");
    let start = repo.revparse_single("refs/heads/up~1").unwrap().id();
    let up = repo.revparse_single("refs/heads/up").unwrap().id();
    branch_at(&repo, "behind", start);
    checkout(&repo, "behind");
    let out = git_rebase_blocking(p(&dir), "up".into(), false, false).unwrap();
    assert_eq!(out.result, "rebased");
    let head = repo.head().unwrap();
    assert_eq!(head.shorthand().ok(), Some("behind"));
    assert_eq!(head.target().unwrap(), up);
    // At up now: rebasing again finds nothing to do.
    assert_eq!(
        git_rebase_blocking(p(&dir), "up".into(), false, false)
            .unwrap()
            .result,
        "up-to-date"
    );
}

#[test]
fn a_rebase_onto_a_remote_branch_stops_at_conflicts_and_names_it() {
    let (dir, repo) = diverged("one\nOURS\nthree\n", "one\nTHEIRS\nthree\n");
    let up = repo.revparse_single("refs/heads/up").unwrap().id();
    repo.reference("refs/remotes/origin/release", up, true, "test")
        .unwrap();
    let out = git_rebase_blocking(p(&dir), "origin/release".into(), true, false).unwrap();
    assert_eq!(out.result, "conflicts");
    assert_eq!(out.conflicts, vec!["a.txt".to_string()]);

    let op = current_operation(&repo).unwrap();
    assert_eq!(op.kind, "rebase");
    assert_eq!(op.branch.as_deref(), Some("main"));
    // What it's rebased onto, not main's upstream (up).
    assert_eq!(op.incoming.as_deref(), Some("origin/release"));
    assert_eq!((op.step, op.total), (Some(1), Some(1)));
    let file = git_conflict_file_blocking(p(&dir), "a.txt".into()).unwrap();
    assert_eq!(file.ours_label, "Upstream (origin/release)");
    assert_eq!(file.theirs_label, "Your commit: ours");

    git_conflict_resolve_blocking(
        p(&dir),
        "a.txt".into(),
        Some("one\nBOTH\nthree\n".into()),
        None,
    )
    .unwrap();
    let out = git_operation_continue_blocking(p(&dir), None).unwrap();
    assert_eq!(out.result, "rebased");
    assert_eq!(repo.state(), RepositoryState::Clean);
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(commit.parent_id(0).unwrap(), up);
    assert_eq!(
        fs::read_to_string(dir.join("a.txt")).unwrap(),
        "one\nBOTH\nthree\n"
    );
}

#[test]
fn a_rebase_that_would_rewrite_pushed_commits_asks_first() {
    let (dir, repo) = diverged("one\ntwo\nthree\nours\n", "zero\none\ntwo\nthree\n");
    // main's commit is on its upstream: pushed.
    let main = repo.head().unwrap().target().unwrap();
    branch_at(&repo, "pushed", main);
    repo.config()
        .unwrap()
        .set_str("branch.main.merge", "refs/heads/pushed")
        .unwrap();
    let out = git_rebase_blocking(p(&dir), "up".into(), false, false).unwrap();
    assert_eq!(out.result, "rewrites-pushed");
    assert_eq!(repo.head().unwrap().target().unwrap(), main);
    assert_eq!(repo.state(), RepositoryState::Clean);
    // Asked, and told to go ahead.
    let out = git_rebase_blocking(p(&dir), "up".into(), false, true).unwrap();
    assert_eq!(out.result, "rebased");
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    assert_ne!(commit.id(), main);
    assert_eq!(commit.summary().unwrap(), Some("ours"));
}

#[test]
fn a_rebase_is_refused_where_it_cant_be_done() {
    let (dir, repo) = diverged("one\ntwo\nthree\nours\n", "zero\none\ntwo\nthree\n");
    let refused = |onto: &str, remote: bool| {
        git_rebase_blocking(p(&dir), onto.into(), remote, false).unwrap_err()
    };
    assert!(refused("main", false).contains("onto itself"));
    assert!(refused("nope", false).contains("no branch nope"));
    // `up` is local: there's no remote branch of that name.
    assert!(refused("up", true).contains("no branch up"));

    let main = repo.head().unwrap().target().unwrap();
    fs::write(dir.join("b.txt"), "changed\n").unwrap();
    let dirty = refused("up", false);
    assert!(dirty.contains("then rebase again"), "{dirty}");
    fs::write(dir.join("b.txt"), "b\n").unwrap();

    repo.set_head_detached(main).unwrap();
    assert!(refused("up", false).contains("Check out a branch"));
    checkout(&repo, "main");
    assert_eq!(repo.head().unwrap().target().unwrap(), main);

    let (dir, repo) = diverged("one\nOURS\nthree\n", "one\nTHEIRS\nthree\n");
    let merging = integrate_upstream(&repo, Some(PullStrategy::Merge), false).unwrap();
    assert_eq!(merging.result, "conflicts");
    let busy = git_rebase_blocking(p(&dir), "up".into(), false, false).unwrap_err();
    assert!(busy.contains("A merge is in progress"), "{busy}");
}

/// The hooks a pull, a merge and a rebase run, as git runs them.
#[cfg(unix)]
mod hooks {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Hook `name` in `dir`'s repository, a shell script running `body`.
    fn hook(repo: &Repository, dir: &Path, name: &str, body: &str) {
        // Whatever the machine's global config says.
        repo.config()
            .unwrap()
            .set_str("core.hooksPath", ".git/hooks")
            .unwrap();
        let file = dir.join(".git/hooks").join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn noted(dir: &Path, file: &str) -> String {
        fs::read_to_string(dir.join(".git").join(file)).unwrap_or_default()
    }

    #[test]
    fn a_pull_that_fast_forwards_runs_post_merge() {
        let (dir, repo) = diverged("one\ntwo\nthree\n", "one\ntwo\nthree\nfour\n");
        // main back at the start: the pull just moves it up.
        let start = repo.revparse_single("main~1").unwrap().id();
        repo.reference("refs/heads/main", start, true, "test")
            .unwrap();
        checkout(&repo, "main");
        hook(
            &repo,
            &dir,
            "post-merge",
            "echo \"$@\" > .git/post-merge-args",
        );
        let out = integrate_upstream(&repo, None, false).unwrap();
        assert_eq!(out.result, "fast-forward");
        assert_eq!(noted(&dir, "post-merge-args"), "0\n");
    }

    #[test]
    fn a_pull_that_merges_runs_the_merge_hooks() {
        let (dir, repo) = diverged("one\ntwo\nthree\nours\n", "zero\none\ntwo\nthree\n");
        hook(&repo, &dir, "pre-merge-commit", "echo pre >> .git/ran");
        hook(&repo, &dir, "prepare-commit-msg", "echo \"$2\" >> .git/ran");
        hook(&repo, &dir, "post-merge", "echo post >> .git/ran");
        hook(&repo, &dir, "pre-commit", "echo pre-commit >> .git/ran");
        let out = integrate_upstream(&repo, Some(PullStrategy::Merge), false).unwrap();
        assert_eq!(out.result, "merged");
        assert_eq!(noted(&dir, "ran"), "pre\nmerge\npost\n");
        assert_eq!(head_commit(&repo).unwrap().parent_count(), 2);
    }

    #[test]
    fn a_merge_its_hook_refuses_stays_in_progress_to_continue() {
        let (dir, repo) = diverged("one\ntwo\nthree\nours\n", "zero\none\ntwo\nthree\n");
        let ours = head_commit(&repo).unwrap().id();
        hook(
            &repo,
            &dir,
            "pre-merge-commit",
            "echo 'not yet' >&2; exit 1",
        );
        let err = integrate_upstream(&repo, Some(PullStrategy::Merge), false).unwrap_err();
        assert!(
            err.starts_with(
                "Not committed: the pre-merge-commit hook failed (exit code 1):\nnot yet\n"
            ),
            "{err}"
        );
        assert_eq!(repo.state(), RepositoryState::Merge);
        assert_eq!(head_commit(&repo).unwrap().id(), ours);

        // Continuing is a commit, with the commit hooks.
        hook(&repo, &dir, "pre-commit", "echo pre-commit >> .git/ran");
        hook(&repo, &dir, "post-commit", "echo post-commit >> .git/ran");
        let out = git_operation_continue_blocking(p(&dir), None).unwrap();
        assert_eq!(out.result, "merged");
        assert_eq!(noted(&dir, "ran"), "pre-commit\npost-commit\n");
        assert_eq!(repo.state(), RepositoryState::Clean);
    }

    #[test]
    fn a_merge_its_hook_refused_can_be_committed_without_the_checks() {
        let (dir, repo) = diverged("one\ntwo\nthree\nours\n", "zero\none\ntwo\nthree\n");
        hook(&repo, &dir, "pre-merge-commit", "exit 1");
        hook(&repo, &dir, "pre-commit", "exit 1");
        hook(&repo, &dir, "post-commit", "echo post-commit >> .git/ran");
        assert!(integrate_upstream(&repo, Some(PullStrategy::Merge), false).is_err());
        assert!(git_operation_continue_blocking(p(&dir), None).is_err());
        let out = git_operation_continue_blocking_with(p(&dir), None, true).unwrap();
        assert_eq!(out.result, "merged");
        assert_eq!(noted(&dir, "ran"), "post-commit\n");
        assert_eq!(head_commit(&repo).unwrap().parent_count(), 2);
    }

    #[test]
    fn pre_rebase_can_refuse_a_rebase() {
        let (dir, repo) = diverged("one\ntwo\nthree\nours\n", "zero\none\ntwo\nthree\n");
        let ours = head_commit(&repo).unwrap().id();
        hook(
            &repo,
            &dir,
            "pre-rebase",
            "echo \"no rebasing onto $1\"; exit 1",
        );
        let err = integrate_upstream(&repo, Some(PullStrategy::Rebase), false).unwrap_err();
        assert_eq!(
            err,
            "Not rebased: the pre-rebase hook failed (exit code 1):\nno rebasing onto up"
        );
        assert_eq!(repo.state(), RepositoryState::Clean);
        assert_eq!(head_commit(&repo).unwrap().id(), ours);
    }

    #[test]
    fn post_rewrite_gets_each_rewritten_commit() {
        let (dir, repo) = diverged("one\ntwo\nthree\nours\n", "zero\none\ntwo\nthree\n");
        let ours = head_commit(&repo).unwrap().id();
        hook(
            &repo,
            &dir,
            "post-rewrite",
            "echo \"$1\" > .git/rewrite-args; cat > .git/rewritten-list",
        );
        let out = integrate_upstream(&repo, Some(PullStrategy::Rebase), false).unwrap();
        assert_eq!(out.result, "rebased");
        let new = head_commit(&repo).unwrap().id();
        assert_ne!(new, ours);
        assert_eq!(noted(&dir, "rewrite-args"), "rebase\n");
        assert_eq!(noted(&dir, "rewritten-list"), format!("{ours} {new}\n"));
    }
}

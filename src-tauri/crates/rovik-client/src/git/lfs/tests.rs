use super::*;
use crate::git::{git_commit_blocking, git_push_blocking_with, git_status_blocking};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

struct Fixture {
    dir: tempfile::TempDir,
    repo: Repository,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "LFS test").unwrap();
            cfg.set_str("user.email", "lfs@rovik.invalid").unwrap();
            cfg.set_str("core.hooksPath", ".hooks").unwrap();
            cfg.set_str("core.attributesFile", "/dev/null").unwrap();
            // A clone can have LFS attributes without local/global install.
            cfg.set_str("filter.lfs.process", "").unwrap();
            cfg.set_str("filter.lfs.clean", "").unwrap();
            cfg.set_bool("filter.lfs.required", false).unwrap();
        }
        repo.set_head("refs/heads/main").unwrap();
        Self { dir, repo }
    }

    fn path(&self) -> String {
        self.dir.path().to_string_lossy().into_owned()
    }

    fn write(&self, path: &str, content: impl AsRef<[u8]>) {
        let target = self.dir.path().join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, content).unwrap();
    }

    fn track(&self) {
        self.write(
            ".gitattributes",
            "*.bin filter=lfs diff=lfs merge=lfs -text\n",
        );
    }

    fn commit(&self) {
        git_commit_blocking(self.path(), "assets".into()).unwrap();
    }

    fn blob(&self, path: &str) -> Vec<u8> {
        let tree = self.repo.head().unwrap().peel_to_tree().unwrap();
        let id = tree.get_path(Path::new(path)).unwrap().id();
        self.repo.find_blob(id).unwrap().content().to_vec()
    }

    fn remote(&self) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        Repository::init_bare(dir.path()).unwrap();
        self.repo
            .remote("origin", dir.path().to_str().unwrap())
            .unwrap();
        dir
    }

    fn hook(&self, name: &str, script: &str) {
        self.write(&format!(".hooks/{name}"), format!("#!/bin/sh\n{script}\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                self.dir.path().join(format!(".hooks/{name}")),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        self.write(".git/info/exclude", ".hooks/\n");
    }

    fn raw_commit(&self) {
        let mut index = self.repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.update_all(["*"], None).unwrap();
        index.write().unwrap();
        let tree = self.repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = self.repo.signature().unwrap();
        let parent = self.repo.head().ok().map(|h| h.peel_to_commit().unwrap());
        self.repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "raw legacy commit",
                &tree,
                &parent.iter().collect::<Vec<_>>(),
            )
            .unwrap();
    }
}

fn real_lfs() {
    assert_eq!(
        std::env::var("ROVIK_REAL_GIT_LFS").as_deref(),
        Ok("1"),
        "Set ROVIK_REAL_GIT_LFS=1 to run Git/LFS acceptance tests"
    );
    let out = rovik_core::process::command("git")
        .args(["lfs", "version"])
        .output()
        .unwrap();
    assert!(out.status.success(), "Git LFS must be installed");
}

fn object_path(root: &Path, bytes: &[u8]) -> PathBuf {
    let oid = sha256(bytes);
    root.join("lfs/objects")
        .join(&oid[..2])
        .join(&oid[2..4])
        .join(oid)
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn status_parses_renames_conflicts_and_unusual_paths() {
    let files = parse_status(b"R  new\tname\n.bin\0old.bin\0?? image.bin\0UU conflict.bin\0 M changed.bin\0D  deleted.bin\0").unwrap();
    assert_eq!(files.len(), 5);
    assert_eq!(files[0].path, "new\tname\n.bin");
    assert!(files[0].staged);
    assert!(!files[0].unstaged);
    assert_eq!(files[1].status, "?");
    assert_eq!(files[2].status, "U");
    assert!(files[3].unstaged);
    assert_eq!(files[4].status, "D");
    assert!(parse_status(b"R  incomplete\0").is_err());
}

#[test]
fn native_scope_preserves_brackets_and_literal_backslashes() {
    assert_eq!(
        native_scope(r"packages/\[app\]"),
        ":(top,literal)packages/[app]"
    );
    assert_eq!(native_scope(r"a\\b\*\?"), r":(top,literal)a\b*?");
}

#[test]
fn attributes_detect_lfs_without_a_global_install_and_reject_raw_index_entries() {
    let f = Fixture::new();
    f.write(".git/info/attributes", "*.bin filter=lfs\n");
    f.write("asset.bin", b"raw\0asset");
    assert!(uses_lfs(&f.repo).unwrap());
    let mut index = f.repo.index().unwrap();
    index.add_path(Path::new("asset.bin")).unwrap();
    index.write().unwrap();
    let error = check_index(&f.repo, &index).unwrap_err();
    assert!(error.contains("asset.bin"));
    assert!(error.contains("full contents"));
}

#[test]
fn ordinary_repositories_and_gitlinks_do_not_require_lfs() {
    let f = Fixture::new();
    f.write("image.png", b"ordinary binary\0");
    f.raw_commit();
    let mut index = f.repo.index().unwrap();
    let mut entry = index.get_path(Path::new("image.png"), 0).unwrap();
    entry.path = b"submodule".to_vec();
    entry.mode = 0o160000;
    entry.id = git2::Oid::from_str("1111111111111111111111111111111111111111").unwrap();
    index.add(&entry).unwrap();
    index.write().unwrap();
    assert!(!uses_lfs(&f.repo).unwrap());
}

#[test]
#[ignore = "requires Git and Git LFS; set ROVIK_REAL_GIT_LFS=1"]
fn commit_filters_assets_before_hooks_and_status_reports_hydrated_assets_clean() {
    real_lfs();
    let f = Fixture::new();
    f.track();
    let bytes = vec![0x82; 160 * 1024 * 1024];
    f.write("large asset.bin", &bytes);
    f.write("empty.bin", []);
    f.write("ordinary.png", b"PNG\0regular image");
    f.hook(
        "pre-commit",
        "git show ':large asset.bin' | grep -q '^oid sha256:' || exit 17",
    );
    f.commit();
    let pointer = String::from_utf8(f.blob("large asset.bin")).unwrap();
    assert!(pointer.starts_with(std::str::from_utf8(POINTER_HEADER).unwrap()));
    assert!(pointer.contains(&format!("oid sha256:{}", sha256(&bytes))));
    assert!(pointer.contains("size 167772160\n"));
    assert_eq!(f.blob("ordinary.png"), b"PNG\0regular image");
    assert_eq!(f.blob("empty.bin"), b"");
    assert!(object_path(f.repo.path(), &bytes).is_file());
    assert!(git_status_blocking(f.path()).unwrap().files.is_empty());
    f.write("large asset.bin", b"changed\0binary");
    let files = git_status_blocking(f.path()).unwrap().files;
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "large asset.bin");
    assert!(files[0].unstaged);
    assert_eq!((files[0].additions, files[0].deletions), (0, 0));
    f.commit();
    fs::remove_file(f.dir.path().join("large asset.bin")).unwrap();
    fs::remove_file(f.dir.path().join(".hooks/pre-commit")).unwrap();
    f.commit();
    assert!(f
        .repo
        .index()
        .unwrap()
        .get_path(Path::new("large asset.bin"), 0)
        .is_none());
}

#[test]
#[ignore = "requires Git and Git LFS; set ROVIK_REAL_GIT_LFS=1"]
fn lfs_commit_keeps_staging_inside_a_project_with_glob_characters() {
    real_lfs();
    let f = Fixture::new();
    f.track();
    f.write("packages/[app]/asset.bin", b"inside\0");
    f.write("packages/a/asset.bin", b"outside\0");
    git_commit_blocking(
        f.dir.path().join("packages/[app]").to_str().unwrap().into(),
        "scoped".into(),
    )
    .unwrap();
    assert!(f
        .blob("packages/[app]/asset.bin")
        .starts_with(POINTER_HEADER));
    assert!(f
        .repo
        .index()
        .unwrap()
        .get_path(Path::new("packages/a/asset.bin"), 0)
        .is_none());
}

#[test]
#[ignore = "requires Git and Git LFS; set ROVIK_REAL_GIT_LFS=1"]
fn push_uploads_lfs_without_a_hook_and_a_fresh_clone_hydrates_it() {
    real_lfs();
    let f = Fixture::new();
    f.track();
    let bytes = b"asset\0contents";
    f.write("asset.bin", bytes);
    f.commit();
    let remote = f.remote();
    assert_eq!(
        git_push_blocking_with(f.path(), false).unwrap().result,
        "pushed"
    );
    assert_eq!(fs::read(object_path(remote.path(), bytes)).unwrap(), bytes);
    let clone = tempfile::tempdir().unwrap();
    let mut cmd = rovik_core::process::command("git");
    configure(&mut cmd);
    let output = cmd
        .args(["clone", "--branch", "main", "--"])
        .arg(remote.path())
        .arg(clone.path().join("clone"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(clone.path().join("clone/asset.bin")).unwrap(),
        bytes
    );
}

#[test]
#[ignore = "requires Git and Git LFS; set ROVIK_REAL_GIT_LFS=1"]
fn custom_hook_failure_blocks_push_and_no_verify_still_uploads_lfs() {
    real_lfs();
    let f = Fixture::new();
    f.track();
    let bytes = b"asset\0contents";
    f.write("asset.bin", bytes);
    f.commit();
    let remote = f.remote();
    f.hook("pre-push", "echo custom-check >&2; exit 1");
    let original = fs::read(f.dir.path().join(".hooks/pre-push")).unwrap();
    let error = git_push_blocking_with(f.path(), false).unwrap_err();
    assert!(
        error.starts_with("Not pushed: the pre-push hook failed"),
        "{error}"
    );
    assert!(Repository::open_bare(remote.path())
        .unwrap()
        .find_reference("refs/heads/main")
        .is_err());
    assert_eq!(
        git_push_blocking_with(f.path(), true).unwrap().result,
        "pushed"
    );
    assert_eq!(fs::read(object_path(remote.path(), bytes)).unwrap(), bytes);
    assert_eq!(
        fs::read(f.dir.path().join(".hooks/pre-push")).unwrap(),
        original
    );
}

#[test]
#[ignore = "requires Git and Git LFS; set ROVIK_REAL_GIT_LFS=1"]
fn missing_lfs_objects_block_push_even_when_hooks_are_skipped() {
    real_lfs();
    let f = Fixture::new();
    f.track();
    let bytes = b"asset\0contents";
    f.write("asset.bin", bytes);
    f.commit();
    let remote = f.remote();
    fs::remove_file(object_path(f.repo.path(), bytes)).unwrap();
    f.repo
        .config()
        .unwrap()
        .set_bool("lfs.allowincompletepush", true)
        .unwrap();
    let error = git_push_blocking_with(f.path(), true).unwrap_err();
    assert!(error.contains("Git LFS failed"), "{error}");
    assert!(!error.starts_with("Not pushed: the pre-push hook"));
    assert!(Repository::open_bare(remote.path())
        .unwrap()
        .find_reference("refs/heads/main")
        .is_err());
}

#[test]
#[ignore = "requires Git and Git LFS; set ROVIK_REAL_GIT_LFS=1"]
fn raw_lfs_files_in_unpublished_commits_block_push_without_rewriting_history() {
    real_lfs();
    let f = Fixture::new();
    f.track();
    f.write("asset.bin", b"legacy raw\0asset");
    f.raw_commit();
    // A repaired tip does not remove the raw object from outgoing history.
    f.write("asset.bin", b"repaired\0asset");
    f.commit();
    let head = f.repo.head().unwrap().target().unwrap();
    let remote = f.remote();
    let error = git_push_blocking_with(f.path(), true).unwrap_err();
    assert!(error.contains("raw LFS-tracked file"), "{error}");
    assert_eq!(f.repo.head().unwrap().target().unwrap(), head);
    assert!(Repository::open_bare(remote.path())
        .unwrap()
        .find_reference("refs/heads/main")
        .is_err());
}

#[test]
#[ignore = "requires Git and Git LFS; set ROVIK_REAL_GIT_LFS=1"]
fn a_commit_hook_cannot_replace_an_lfs_pointer_with_raw_contents() {
    real_lfs();
    let f = Fixture::new();
    f.track();
    f.write("asset.bin", b"asset\0contents");
    f.hook("pre-commit", "oid=$(git hash-object -w --no-filters asset.bin)\ngit update-index --cacheinfo \"100644,$oid,asset.bin\"");
    let error = git_commit_blocking(f.path(), "must fail".into()).unwrap_err();
    assert!(
        error.contains("index contains its full contents"),
        "{error}"
    );
    assert!(f.repo.head().is_err());
}

#[test]
#[ignore = "requires Git and Git LFS; set ROVIK_REAL_GIT_LFS=1"]
fn conflict_resolution_uses_the_lfs_clean_filter() {
    real_lfs();
    let f = Fixture::new();
    f.track();
    f.write("asset.bin", b"asset\0contents");
    f.commit();
    crate::git_sync::git_conflict_resolve_blocking(
        f.path(),
        "asset.bin".into(),
        Some("resolved contents".into()),
        None,
    )
    .unwrap();
    let index = f.repo.index().unwrap();
    let entry = index.get_path(Path::new("asset.bin"), 0).unwrap();
    assert!(is_pointer(&f.repo, entry.id).unwrap());
    assert_eq!(
        fs::read(f.dir.path().join("asset.bin")).unwrap(),
        b"resolved contents"
    );
}

#[test]
#[ignore = "requires Git and Git LFS; set ROVIK_REAL_GIT_LFS=1"]
fn push_respects_pushurl_and_an_installed_lfs_hook() {
    real_lfs();
    let f = Fixture::new();
    f.track();
    let bytes = b"asset\0contents";
    f.write("asset.bin", bytes);
    f.commit();
    let fetch_remote = f.remote();
    let push_remote = tempfile::tempdir().unwrap();
    Repository::init_bare(push_remote.path()).unwrap();
    f.repo
        .remote_set_pushurl("origin", push_remote.path().to_str())
        .unwrap();
    f.hook(
        "pre-push",
        "cat > .git/hook-refs\ngit lfs pre-push \"$@\" < .git/hook-refs",
    );
    assert_eq!(
        git_push_blocking_with(f.path(), false).unwrap().result,
        "pushed"
    );
    assert_eq!(
        fs::read(object_path(push_remote.path(), bytes)).unwrap(),
        bytes
    );
    assert!(!object_path(fetch_remote.path(), bytes).exists());
    let tip = f.repo.head().unwrap().target().unwrap();
    assert_eq!(
        fs::read_to_string(f.repo.path().join("hook-refs")).unwrap(),
        format!(
            "refs/heads/main {tip} refs/heads/main {}\n",
            git2::Oid::ZERO_SHA1
        )
    );
}

#[test]
#[ignore = "requires Git and Git LFS; set ROVIK_REAL_GIT_LFS=1"]
fn push_uploads_objects_reachable_only_from_deleted_historical_assets() {
    real_lfs();
    let f = Fixture::new();
    f.track();
    let bytes = b"deleted\0asset";
    f.write("asset.bin", bytes);
    f.commit();
    fs::remove_file(f.dir.path().join("asset.bin")).unwrap();
    fs::remove_file(f.dir.path().join(".gitattributes")).unwrap();
    f.commit();
    let remote = f.remote();
    assert_eq!(
        git_push_blocking_with(f.path(), false).unwrap().result,
        "pushed"
    );
    assert_eq!(fs::read(object_path(remote.path(), bytes)).unwrap(), bytes);
}

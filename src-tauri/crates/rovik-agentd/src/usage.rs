//! What a chat's worktree holds, by kind: its source (what git doesn't
//! ignore, the user's edits among it), installed dependencies, build
//! output, and the other files git ignores. The hosts page lists each
//! chat's (`ChatStorage`), and the cleanup removes the two rebuildable
//! kinds (generated.rs).
//!
//! Sizes are the blocks the files take on disk, a file with several hard
//! links counted once: a package manager's links into its store, or
//! Cargo's between `deps/` and the binaries, don't count twice. Blocks a
//! copy-on-write file system shares between two chats' files can't be
//! seen this way, and count for each.

use git2::{Index, Repository};
use std::collections::{HashMap, HashSet};
use std::fs::{self, Metadata};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// The first line of a `CACHEDIR.TAG`, which marks a folder whose contents
/// a tool made and makes again (the Cache Directory Tagging spec): Cargo
/// writes it into `target/`.
const CACHE_TAG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// What a worktree's files take on disk, in bytes, by kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// What git doesn't ignore: tracked files and the user's untracked ones.
    pub source: u64,
    /// Installed dependencies (`node_modules` of a package).
    pub dependencies: u64,
    /// Build output (a folder tagged as a cache, as Cargo's `target/`).
    pub build: u64,
    /// The other files git ignores: logs, `.env`, caches untagged.
    pub other: u64,
}

/// A folder a tool fills and fills again: removing it loses nothing a
/// rebuild or a reinstall doesn't bring back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dependencies,
    Build,
}

/// The kind of what's in `dir`, when it's one a tool makes: `node_modules`
/// beside a `package.json`, or a folder tagged as a cache. Names alone
/// (`dist/`, `build/`) don't say: a wrong guess would delete the user's
/// files.
pub fn disposable_kind(dir: &Path) -> Option<Kind> {
    let beside_package = dir.file_name().is_some_and(|n| n == "node_modules")
        && dir
            .parent()
            .is_some_and(|p| p.join("package.json").is_file());
    if beside_package {
        return Some(Kind::Dependencies);
    }
    let mut first = [0; CACHE_TAG.len()];
    let tagged = fs::File::open(dir.join("CACHEDIR.TAG"))
        .and_then(|mut tag| tag.read_exact(&mut first))
        .is_ok_and(|()| first == CACHE_TAG);
    tagged.then_some(Kind::Build)
}

/// A worktree's git rules: what it ignores, and what it tracks anyway.
pub struct Git {
    repo: Option<(Repository, Index)>,
}

impl Git {
    /// The rules of the repository at `worktree`; without one, nothing is
    /// ignored.
    pub fn open(worktree: &Path) -> Git {
        let repo = Repository::open(worktree)
            .and_then(|repo| repo.index().map(|index| (repo, index)))
            .map_err(|e| log::warn!("{}'s git rules: {e}", worktree.display()));
        Git { repo: repo.ok() }
    }

    /// Whether git ignores `rel` (relative to the worktree) and tracks
    /// nothing at or under it. Unknown, it says no.
    pub fn ignores(&self, rel: &Path, dir: bool) -> bool {
        let Some((repo, index)) = &self.repo else {
            return false;
        };
        let Some(text) = rel.to_str() else {
            return false;
        };
        let tracked = match dir {
            true => index.find_prefix(format!("{text}/")).is_ok(),
            false => index.get_path(rel, 0).is_some(),
        };
        !tracked && repo.is_path_ignored(rel).unwrap_or(false)
    }
}

/// Which of `Usage`'s kinds a file counts in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bucket {
    Source,
    Dependencies,
    Build,
    Other,
}

impl From<Kind> for Bucket {
    fn from(kind: Kind) -> Bucket {
        match kind {
            Kind::Dependencies => Bucket::Dependencies,
            Kind::Build => Bucket::Build,
        }
    }
}

/// What `worktree` holds, by kind, measured now: links not followed, other
/// file systems not entered. What can't be read counts as nothing.
pub fn measure(worktree: &Path) -> Usage {
    let mut usage = Usage::default();
    let Ok(top) = fs::symlink_metadata(worktree) else {
        return usage;
    };
    let git = Git::open(worktree);
    let mut seen = HashSet::new();
    // A folder, and the kind all of it counts in when one is known.
    let mut pending = vec![(worktree.to_path_buf(), None)];
    while let Some((folder, fixed)) = pending.pop() {
        for entry in fs::read_dir(&folder).into_iter().flatten().flatten() {
            // A `DirEntry`'s metadata doesn't follow a link.
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let path = entry.path();
            let bucket = fixed.unwrap_or_else(|| {
                let rel = path.strip_prefix(worktree).unwrap_or(&path);
                classify(&git, &path, rel, &meta)
            });
            let size = allocated(&meta, &mut seen);
            match bucket {
                Bucket::Source => usage.source += size,
                Bucket::Dependencies => usage.dependencies += size,
                Bucket::Build => usage.build += size,
                Bucket::Other => usage.other += size,
            }
            if meta.is_dir() && meta.dev() == top.dev() {
                // Below source, each file is asked again; not in `.git`,
                // which git counts as ignored.
                let git_dir = path == worktree.join(".git");
                let below = match fixed {
                    Some(_) => fixed,
                    None => (bucket != Bucket::Source || git_dir).then_some(bucket),
                };
                pending.push((path, below));
            }
        }
    }
    usage
}

/// What `dir` and everything in it take on disk, as `measure` counts it.
pub fn allocated_in(dir: &Path) -> u64 {
    let mut seen = HashSet::new();
    let mut total = 0;
    walk(dir, |meta| total += allocated(meta, &mut seen));
    total
}

/// What removing `dir` would free: as `allocated_in`, but a file with a
/// link outside `dir` (a package manager's store, say) frees nothing.
pub fn freeable_in(dir: &Path) -> u64 {
    let mut total = 0;
    // Files with more links: how many of them are in `dir`, of how many.
    let mut linked: HashMap<(u64, u64), (u64, u64, u64)> = HashMap::new();
    walk(dir, |meta| match meta.nlink() > 1 && !meta.is_dir() {
        true => {
            let entry = (linked.entry((meta.dev(), meta.ino()))).or_insert((
                0,
                meta.nlink(),
                meta.blocks() * 512,
            ));
            entry.0 += 1;
        }
        false => total += meta.blocks() * 512,
    });
    let all_inside = linked.values().filter(|(seen, links, _)| seen >= links);
    total + all_inside.map(|(_, _, size)| size).sum::<u64>()
}

/// Calls `f` with the metadata of `dir` and of everything in it: links not
/// followed, other file systems not entered.
fn walk(dir: &Path, mut f: impl FnMut(&Metadata)) {
    let Ok(top) = fs::symlink_metadata(dir) else {
        return;
    };
    f(&top);
    let mut pending = match top.is_dir() {
        true => vec![dir.to_path_buf()],
        false => Vec::new(),
    };
    while let Some(folder) = pending.pop() {
        for entry in fs::read_dir(&folder).into_iter().flatten().flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            f(&meta);
            if meta.is_dir() && meta.dev() == top.dev() {
                pending.push(entry.path());
            }
        }
    }
}

/// The kind of `path` (`rel` in its worktree), asked of it alone.
fn classify(git: &Git, path: &Path, rel: &Path, meta: &Metadata) -> Bucket {
    if rel == Path::new(".git") {
        return Bucket::Source;
    }
    if let Some(kind) = meta.is_dir().then_some(path).and_then(disposable_kind) {
        return kind.into();
    }
    match git.ignores(rel, meta.is_dir()) {
        true => Bucket::Other,
        false => Bucket::Source,
    }
}

/// The bytes a file's blocks take, or none for a file with more links
/// whose blocks were counted already.
fn allocated(meta: &Metadata, seen: &mut HashSet<(u64, u64)>) -> u64 {
    if meta.nlink() > 1 && !meta.is_dir() && !seen.insert((meta.dev(), meta.ino())) {
        return 0;
    }
    meta.blocks() * 512
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use rovik_core::testing::temp_dir;
    use std::path::PathBuf;

    /// A repository at `dir` ignoring `ignore`, with `tracked` committed.
    pub fn repository(dir: &Path, ignore: &str, tracked: &[(&str, &str)]) -> Repository {
        let repo = Repository::init(dir).unwrap();
        fs::write(dir.join(".gitignore"), ignore).unwrap();
        let mut index = repo.index().unwrap();
        for (path, text) in tracked {
            write(&dir.join(path), text);
            index.add_path(Path::new(path)).unwrap();
        }
        index.add_path(Path::new(".gitignore")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let me = git2::Signature::now("Rovik", "rovik@example.com").unwrap();
        repo.commit(Some("HEAD"), &me, &me, "Start", &tree, &[])
            .unwrap();
        drop(tree);
        repo
    }

    pub fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// A folder tagged as a cache, as Cargo tags `target/`.
    pub fn tag(dir: &Path) {
        write(
            &dir.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n# made by cargo\n",
        );
    }

    /// The blocks the paths take, each counted.
    fn blocks(paths: &[PathBuf]) -> u64 {
        paths
            .iter()
            .map(|p| fs::symlink_metadata(p).unwrap().blocks() * 512)
            .sum()
    }

    /// Some kilobytes: a file whose blocks aren't nothing.
    fn kb(n: usize) -> String {
        "x".repeat(n << 10)
    }

    #[test]
    fn node_modules_beside_package_json_counts_as_dependencies() {
        let dir = temp_dir();
        let root = &dir.0;
        repository(root, "node_modules/\n", &[("package.json", "{}")]);
        write(&root.join("node_modules/left-pad/index.js"), &kb(8));

        let modules = root.join("node_modules");
        let usage = measure(root);
        assert_eq!(
            usage.dependencies,
            blocks(&[
                modules.clone(),
                modules.join("left-pad"),
                modules.join("left-pad/index.js")
            ])
        );
        assert_eq!(disposable_kind(&modules), Some(Kind::Dependencies));
        assert_eq!(usage.build, 0);
    }

    #[test]
    fn node_modules_without_package_json_is_not_dependencies() {
        let dir = temp_dir();
        let root = &dir.0;
        repository(root, "node_modules/\n", &[]);
        write(&root.join("node_modules/x.js"), &kb(4));

        assert_eq!(disposable_kind(&root.join("node_modules")), None);
        let usage = measure(root);
        assert_eq!(usage.dependencies, 0);
        assert!(usage.other > 0, "ignored, so other: {usage:?}");
    }

    #[test]
    fn a_folder_tagged_as_a_cache_counts_as_build_output() {
        let dir = temp_dir();
        let root = &dir.0;
        repository(root, "target/\n", &[("Cargo.toml", "[package]")]);
        let target = root.join("target");
        tag(&target);
        write(&target.join("debug/app"), &kb(16));

        let usage = measure(root);
        assert_eq!(
            usage.build,
            blocks(&[
                target.clone(),
                target.join("CACHEDIR.TAG"),
                target.join("debug"),
                target.join("debug/app")
            ])
        );
        assert_eq!(disposable_kind(&target), Some(Kind::Build));
    }

    #[test]
    fn a_tag_without_the_signature_is_not_build_output() {
        let dir = temp_dir();
        let root = &dir.0;
        repository(root, "out/\n", &[]);
        write(
            &root.join("out/CACHEDIR.TAG"),
            "Signature: something else\n",
        );
        write(&root.join("out/report.txt"), &kb(4));

        assert_eq!(disposable_kind(&root.join("out")), None);
        let usage = measure(root);
        assert_eq!(usage.build, 0);
        assert!(usage.other > 0, "{usage:?}");
    }

    #[test]
    fn ignored_files_elsewhere_count_as_other() {
        let dir = temp_dir();
        let root = &dir.0;
        repository(root, ".env\nlogs/\n", &[]);
        write(&root.join(".env"), &kb(4));
        write(&root.join("logs/run.log"), &kb(4));

        let usage = measure(root);
        assert_eq!(
            usage.other,
            blocks(&[
                root.join(".env"),
                root.join("logs"),
                root.join("logs/run.log")
            ])
        );
    }

    #[test]
    fn edits_and_untracked_files_count_as_source() {
        let dir = temp_dir();
        let root = &dir.0;
        repository(root, "*.log\n", &[("src/main.rs", "fn main() {}")]);
        write(&root.join("src/main.rs"), &kb(8));
        write(&root.join("notes.txt"), &kb(4));
        // Tracked though ignored: still source.
        write(&root.join("keep.log"), &kb(4));
        let repo = Repository::open(root).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("keep.log")).unwrap();
        index.write().unwrap();

        let usage = measure(root);
        assert!(
            usage.source >= blocks(&[root.join("src/main.rs"), root.join("notes.txt")]),
            "{usage:?}"
        );
        assert_eq!(usage.other, 0, "{usage:?}");
    }

    #[test]
    fn a_hard_linked_file_counts_once() {
        let dir = temp_dir();
        let root = &dir.0;
        repository(root, "node_modules/\n", &[("package.json", "{}")]);
        let modules = root.join("node_modules");
        write(&modules.join("a.js"), &kb(16));
        fs::hard_link(modules.join("a.js"), modules.join("b.js")).unwrap();

        let usage = measure(root);
        assert_eq!(
            usage.dependencies,
            blocks(&[modules.clone(), modules.join("a.js")])
        );
    }

    #[test]
    fn a_file_linked_from_outside_frees_nothing_when_its_folder_goes() {
        let dir = temp_dir();
        let store = dir.0.join("store/a.js");
        let modules = dir.0.join("node_modules");
        write(&store, &kb(64));
        write(&modules.join("own.js"), &kb(16));
        fs::create_dir_all(modules.join("pkg")).unwrap();
        fs::hard_link(&store, modules.join("pkg/a.js")).unwrap();
        fs::hard_link(modules.join("own.js"), modules.join("pkg/own.js")).unwrap();

        let freeable = freeable_in(&modules);
        assert_eq!(
            freeable,
            blocks(&[modules.clone(), modules.join("pkg"), modules.join("own.js")])
        );
        assert!(allocated_in(&modules) >= freeable + (64 << 10));
    }

    #[test]
    fn a_link_is_not_followed() {
        let dir = temp_dir();
        let root = dir.0.join("worktree");
        let outside = dir.0.join("outside");
        write(&outside.join("big.bin"), &kb(64));
        repository(&root, "", &[]);
        let total = |u: Usage| u.source + u.dependencies + u.build + u.other;
        let before = total(measure(&root));
        std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();

        let after = total(measure(&root));
        assert!(after < before + (64 << 10), "{before} {after}");
    }
}

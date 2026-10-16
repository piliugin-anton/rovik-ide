//! The dependencies a project reaches, loaded breadth-first from what names
//! them: the packages its sources import (the seeds), then the packages
//! those packages' declarations import, and so on, each package once under
//! its real folder. A package is its `package.json` files, which TypeScript
//! reads to resolve a bare import through `types` and `exports`, and every
//! `*.d.ts` (`.d.mts`, `.d.cts`) under it, nested `node_modules` aside:
//! those hold other packages, loaded when something imports them.
//!
//! Breadth-first, so should a limit be hit, it cuts off what is farthest
//! from the code: a dependency of a dependency, not a package the project
//! imports. The limits are a safety net; a project's closure is usually a
//! few hundred files.

use super::budget::{Collector, Limits, Walk};
use super::imports::{self, candidates};
use super::resolve::{self, Found};
use super::{Link, LoadedFile};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

const LIMITS: Limits = Limits {
    max_files: 40_000,
    max_file_bytes: 16 * 1024 * 1024,
    max_total_bytes: 128 * 1024 * 1024,
};
/// Packages nest their declarations a few folders deep; this only stops a runaway.
const MAX_DEPTH: usize = 40;

/// Folders inside a package that never take part in resolution: caches
/// and binaries, and its tests, docs and examples.
const SKIP_DIRS: &[&str] = &[
    ".bin",
    ".cache",
    ".pnpm",
    ".ignored",
    ".store",
    "test",
    "tests",
    "__tests__",
    "coverage",
    "docs",
    "examples",
    ".github",
];

/// A package the closure loaded.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LoadedPackage {
    /// As imported: `react`, `@types/node`.
    pub name: String,
    /// Its real folder (through links), absolute.
    pub dir: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LoadedTypings {
    pub files: Vec<LoadedFile>,
    /// Links to folders: files are under the target, the path resolves to it.
    pub links: Vec<Link>,
    /// The packages loaded, each once.
    pub packages: Vec<LoadedPackage>,
    /// Seeds no installed package was found for: a true "cannot find module".
    pub missing: Vec<String>,
    /// A limit was hit: some files weren't loaded.
    pub truncated: bool,
}

pub struct Closure {
    collector: Collector,
    links: Vec<Link>,
    linked: HashSet<String>,
    packages: Vec<LoadedPackage>,
    /// Package folders queued or loaded, and those the caller already has.
    visited: HashSet<PathBuf>,
    queue: VecDeque<(String, PathBuf)>,
    /// What each probed `node_modules/<name>` path holds, so a package
    /// imported from hundreds of files is looked up once per folder.
    probed: HashMap<PathBuf, Option<Found>>,
    missing: Vec<String>,
    stopped: bool,
}

impl Closure {
    /// `exclude`: package folders already loaded, not loaded again.
    pub fn new(exclude: impl Iterator<Item = PathBuf>) -> Self {
        Self {
            collector: Collector::new(LIMITS),
            links: Vec::new(),
            linked: HashSet::new(),
            packages: Vec::new(),
            visited: exclude.collect(),
            queue: VecDeque::new(),
            probed: HashMap::new(),
            missing: Vec::new(),
            stopped: false,
        }
    }

    /// A file in `from` imports `specifier`: its package, or its `@types`,
    /// whichever is installed, is queued. Neither: the seed is missing.
    pub fn want(&mut self, from: &Path, specifier: &str) {
        let names = candidates(specifier);
        if names.is_empty() {
            return; // a relative path or an alias: no package to look for
        }
        let mut found_any = false;
        for name in names {
            let found = resolve::find(from, &name, &mut |candidate| {
                self.probed
                    .entry(candidate.to_path_buf())
                    .or_insert_with(|| resolve::entry(candidate))
                    .clone()
            });
            if let Some(found) = found {
                found_any = true;
                self.enqueue(name, found);
            }
        }
        if !found_any && !self.missing.iter().any(|m| m == specifier) {
            self.missing.push(specifier.to_string());
        }
    }

    /// Every `@types` package in this `node_modules` folder: TypeScript
    /// takes them in as globals, whether or not anything imports them.
    pub fn want_types_in(&mut self, node_modules: &Path) {
        let Ok(entries) = fs::read_dir(node_modules.join("@types")) else {
            return;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .filter(|n| !n.starts_with('.'))
            .collect();
        names.sort();
        for name in names {
            if let Some(found) = resolve::entry(&node_modules.join("@types").join(&name)) {
                self.enqueue(format!("@types/{name}"), found);
            }
        }
    }

    fn enqueue(&mut self, name: String, found: Found) {
        if let Some(link) = found.link {
            self.note_link(link);
        }
        if self.visited.insert(found.dir.clone()) {
            self.queue.push_back((name, found.dir));
        }
    }

    fn note_link(&mut self, link: Link) {
        if self.linked.insert(link.path.clone()) {
            self.links.push(link);
        }
    }

    /// Loads every queued package, and what their declarations reach.
    pub fn run(&mut self) {
        while let Some((name, dir)) = self.queue.pop_front() {
            if self.stopped {
                break;
            }
            self.load_package(name, &dir);
        }
    }

    pub fn finish(self) -> LoadedTypings {
        let (files, truncated) = self.collector.finish();
        LoadedTypings {
            files,
            links: self.links,
            packages: self.packages,
            missing: self.missing,
            truncated,
        }
    }

    fn load_package(&mut self, name: String, dir: &Path) {
        self.packages.push(LoadedPackage {
            name,
            dir: dir.to_string_lossy().into_owned(),
        });
        let mut entries = WalkDir::new(dir)
            .follow_links(false)
            .max_depth(MAX_DEPTH)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(wanted);
        while let Some(Ok(entry)) = entries.next() {
            let path = entry.path();
            if entry.file_type().is_symlink() {
                // A linked folder inside a package is another package
                // (`npm link`, a member's dependency): loaded once, under
                // its real path. A linked file is loaded as the file itself.
                match resolve::entry(path) {
                    Some(found) => {
                        let linked = path.file_name().unwrap_or_default().to_string_lossy();
                        self.enqueue(linked.into_owned(), found);
                    }
                    None => {
                        if let Ok(meta) = fs::metadata(path) {
                            if meta.is_file() {
                                self.add_file(path, meta.len());
                            }
                        }
                    }
                }
            } else if entry.file_type().is_dir() {
                // Nested packages are loaded when something imports them.
                if entry.depth() > 0 && entry.file_name() == "node_modules" {
                    entries.skip_current_dir();
                }
            } else if let Ok(meta) = entry.metadata() {
                self.add_file(path, meta.len());
            }
            if self.stopped {
                return;
            }
        }
    }

    fn add_file(&mut self, path: &Path, len: u64) {
        let (content, walk) = self.collector.load(path, len);
        if walk == Walk::Stop {
            self.stopped = true;
            return;
        }
        let specifiers = match content {
            Some(text) if is_declaration(path) => imports::specifiers(text),
            _ => return,
        };
        let dir = path.parent().unwrap_or(path).to_path_buf();
        for specifier in specifiers {
            self.want(&dir, &specifier);
        }
    }
}

fn is_declaration(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    [".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .any(|ext| name.ends_with(ext))
}

fn wanted(entry: &DirEntry) -> bool {
    // Links are looked at by the walk itself (a link to a folder is a
    // folder to it, whatever its name says).
    entry.file_type().is_symlink() || is_typing_entry(entry.file_name(), entry.file_type().is_dir())
}

/// Whether a folder is worth descending into, or a file worth loading.
fn is_typing_entry(name: &OsStr, is_dir: bool) -> bool {
    let name = name.to_string_lossy();
    if is_dir {
        return !SKIP_DIRS.contains(&name.as_ref());
    }
    name == "package.json" || is_declaration(Path::new(name.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typings::test_support::{relative, relative_paths, temp_project};

    fn closure_from(dir: &Path, from: &str, specifiers: &[&str]) -> LoadedTypings {
        let mut closure = Closure::new(std::iter::empty());
        for spec in specifiers {
            closure.want(&dir.join(from), spec);
        }
        closure.run();
        closure.finish()
    }

    #[test]
    fn keeps_typings_and_package_json_but_skips_stores_and_test_dirs() {
        for dir in [
            ".pnpm",
            ".ignored",
            ".store",
            "test",
            "node_modules",
            "solid-js",
        ] {
            assert_eq!(
                is_typing_entry(OsStr::new(dir), true),
                ![".pnpm", ".ignored", ".store", "test"].contains(&dir),
                "dir {dir}"
            );
        }
        for file in ["index.d.ts", "index.d.mts", "index.d.cts", "package.json"] {
            assert!(is_typing_entry(OsStr::new(file), false), "file {file}");
        }
        for file in ["index.js", "index.d.ts.map", "README.md", "index.ts"] {
            assert!(!is_typing_entry(OsStr::new(file), false), "file {file}");
        }
    }

    #[test]
    fn loads_the_imported_packages_and_what_their_declarations_import() {
        let dir = temp_project(
            "closure-walk",
            &[
                ("src/app.ts", ""),
                ("node_modules/a/package.json", "{}"),
                (
                    "node_modules/a/index.d.ts",
                    "export * from './lib/x'; import 'b/sub';",
                ),
                (
                    "node_modules/a/lib/x.d.ts",
                    "/// <reference types=\"c\" />\nexport {}",
                ),
                ("node_modules/a/lib/x.js", "x"),
                ("node_modules/a/test/a.d.ts", "export {}"),
                ("node_modules/a/node_modules/b/types.d.mts", "export {}"),
                ("node_modules/a/node_modules/b/package.json", "{}"),
                (
                    "node_modules/b/index.d.ts",
                    "hoisted, shadowed by a's own b",
                ),
                ("node_modules/@types/c/index.d.ts", "declare var c: any"),
                (
                    "node_modules/.pnpm/a@1/node_modules/a/index.d.ts",
                    "export {}",
                ),
                ("node_modules/unused/index.d.ts", "export {}"),
                ("src/own.d.ts", "export {}"),
            ],
        );
        let loaded = closure_from(&dir, "src", &["a", "./local", "@/alias", "ghost"]);
        assert!(!loaded.truncated);
        assert_eq!(
            relative_paths(&dir, &loaded.files),
            [
                "node_modules/@types/c/index.d.ts",
                "node_modules/a/index.d.ts",
                "node_modules/a/lib/x.d.ts",
                "node_modules/a/node_modules/b/package.json",
                "node_modules/a/node_modules/b/types.d.mts",
                "node_modules/a/package.json",
            ]
        );
        let names: Vec<&str> = loaded.packages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "@types/c"], "breadth-first");
        assert_eq!(
            loaded.missing,
            ["ghost"],
            "an alias isn't a missing package"
        );
    }

    #[test]
    fn workspace_members_have_their_own_dependencies_and_all_types_are_taken_in() {
        let dir = temp_project(
            "closure-workspace",
            &[
                (".gitignore", "node_modules\ndist\n"),
                ("node_modules/shared/index.d.ts", "export {}"),
                ("node_modules/@types/node/index.d.ts", "export {}"),
                ("packages/app/package.json", "{}"),
                (
                    "packages/app/node_modules/only-here/index.d.ts",
                    "export {}",
                ),
                (
                    "packages/app/node_modules/@types/jest/index.d.ts",
                    "export {}",
                ),
                (
                    "packages/app/src/a.ts",
                    "import 'only-here'; import 'shared';",
                ),
            ],
        );
        let mut closure = Closure::new(std::iter::empty());
        closure.want(&dir.join("packages/app/src"), "only-here");
        closure.want(&dir.join("packages/app/src"), "shared");
        closure.want_types_in(&dir.join("node_modules"));
        closure.want_types_in(&dir.join("packages/app/node_modules"));
        closure.run();
        let loaded = closure.finish();
        assert_eq!(
            relative_paths(&dir, &loaded.files),
            [
                "node_modules/@types/node/index.d.ts",
                "node_modules/shared/index.d.ts",
                "packages/app/node_modules/@types/jest/index.d.ts",
                "packages/app/node_modules/only-here/index.d.ts",
            ]
        );
    }

    #[test]
    fn a_project_without_dependencies_loads_nothing() {
        let dir = temp_project("closure-none", &[("src/a.ts", "import 'x';")]);
        let loaded = closure_from(&dir, "src", &["x"]);
        assert!(loaded.files.is_empty() && loaded.packages.is_empty() && !loaded.truncated);
        assert_eq!(loaded.missing, ["x"]);
    }

    #[test]
    fn excluded_packages_are_not_loaded_again() {
        let dir = temp_project(
            "closure-exclude",
            &[
                ("node_modules/a/index.d.ts", "import 'b';"),
                ("node_modules/b/index.d.ts", "export {}"),
            ],
        );
        let mut closure = Closure::new(std::iter::once(dir.join("node_modules/b")));
        closure.want(&dir.join("src"), "a");
        closure.run();
        let loaded = closure.finish();
        assert_eq!(
            relative_paths(&dir, &loaded.files),
            ["node_modules/a/index.d.ts"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn linked_packages_load_once_under_their_real_path_with_the_links_reported() {
        use std::os::unix::fs::symlink;
        let dir = temp_project(
            "closure-links",
            &[
                (".gitignore", "node_modules\n"),
                (
                    "node_modules/.pnpm/foo@1/node_modules/foo/package.json",
                    "{}",
                ),
                (
                    "node_modules/.pnpm/foo@1/node_modules/foo/index.d.ts",
                    "import 'bar';",
                ),
                (
                    "node_modules/.pnpm/bar@2/node_modules/bar/index.d.ts",
                    "export {}",
                ),
                (
                    "node_modules/.pnpm/unused@1/node_modules/unused/index.d.ts",
                    "export {}",
                ),
                ("packages/lib/package.json", "{}"),
                ("packages/lib/dist/index.d.ts", "import 'own';"),
                ("packages/lib/node_modules/own/index.d.ts", "export {}"),
                ("packages/app/package.json", "{}"),
            ],
        );
        // pnpm: the root's and a member's `foo` both link into the store;
        // a workspace member is linked into the root under its name.
        symlink(".pnpm/foo@1/node_modules/foo", dir.join("node_modules/foo")).unwrap();
        // foo's own dependency, linked beside it in the store.
        symlink(
            "../../bar@2/node_modules/bar",
            dir.join("node_modules/.pnpm/foo@1/node_modules/bar"),
        )
        .unwrap();
        fs::create_dir_all(dir.join("packages/app/node_modules")).unwrap();
        symlink(
            "../../../node_modules/.pnpm/foo@1/node_modules/foo",
            dir.join("packages/app/node_modules/foo"),
        )
        .unwrap();
        fs::create_dir_all(dir.join("node_modules/@scope")).unwrap();
        symlink("../../packages/lib", dir.join("node_modules/@scope/lib")).unwrap();
        symlink("missing", dir.join("node_modules/broken")).unwrap();

        let mut closure = Closure::new(std::iter::empty());
        closure.want(&dir.join("src"), "foo");
        closure.want(&dir.join("packages/app/src"), "foo");
        closure.want(&dir.join("src"), "@scope/lib");
        closure.want(&dir.join("src"), "broken");
        closure.run();
        let loaded = closure.finish();
        assert_eq!(
            relative_paths(&dir, &loaded.files),
            [
                "node_modules/.pnpm/bar@2/node_modules/bar/index.d.ts",
                "node_modules/.pnpm/foo@1/node_modules/foo/index.d.ts",
                "node_modules/.pnpm/foo@1/node_modules/foo/package.json",
                "packages/lib/dist/index.d.ts",
                "packages/lib/node_modules/own/index.d.ts",
                "packages/lib/package.json",
            ]
        );
        let mut links: Vec<(String, String)> = loaded
            .links
            .iter()
            .map(|l| (relative(&dir, &l.path), relative(&dir, &l.target)))
            .collect();
        links.sort();
        assert_eq!(
            links,
            [
                (
                    "node_modules/.pnpm/foo@1/node_modules/bar".into(),
                    "node_modules/.pnpm/bar@2/node_modules/bar".into()
                ),
                ("node_modules/@scope/lib".into(), "packages/lib".into()),
                (
                    "node_modules/foo".into(),
                    "node_modules/.pnpm/foo@1/node_modules/foo".into()
                ),
                (
                    "packages/app/node_modules/foo".into(),
                    "node_modules/.pnpm/foo@1/node_modules/foo".into()
                ),
            ]
        );
        assert_eq!(loaded.missing, ["broken"]);
        assert_eq!(
            loaded.packages.iter().filter(|p| p.name == "foo").count(),
            1
        );
    }
}

//! The project's own JavaScript/TypeScript sources, with its tsconfig /
//! jsconfig files (which config owns which file, workspace members'
//! included: src/lib/tsconfig.ts) and package.json files (a file's module
//! format and a package's `exports`, under node16 resolution). Walked the
//! way git sees the project (.gitignore respected), skipping dependencies
//! and build output. The sources are the files of the TypeScript worker's
//! program, so an import of a file no tab has open still resolves.
//!
//! The same walk finds every `node_modules` folder of the project, the
//! root's and workspace members' (gitignored as a rule, so looked for next
//! to each folder visited rather than walked into): where the closure
//! starts from.

use super::budget::{Collector, Limits, Walk};
use super::LoadedFiles;
use std::path::{Path, PathBuf};

const EXTENSIONS: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];
const LIMITS: Limits = Limits {
    max_files: 5_000,
    max_file_bytes: 512 * 1024,
    max_total_bytes: 40 * 1024 * 1024,
};

/// What one walk of the project's folders finds.
pub struct Scan {
    pub files: LoadedFiles,
    /// Every `node_modules` folder in the project, the first folder's first.
    pub node_modules: Vec<PathBuf>,
}

/// tsconfig.json, jsconfig.json and their tsconfig.*.json variants.
pub fn is_config(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    (name.starts_with("tsconfig") || name.starts_with("jsconfig")) && name.ends_with(".json")
}

pub fn is_manifest(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == "package.json")
}

fn is_wanted(path: &Path) -> bool {
    is_config(path)
        || is_manifest(path)
        || path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| EXTENSIONS.contains(&e))
}

/// Walks the project's folders — a project's one, a workspace's several —
/// into one set, so the limits are for the whole project (C06). Folders are
/// walked in the order given; a folder's `node_modules` comes before the next
/// folder's.
pub fn scan(roots: &[PathBuf]) -> Scan {
    walk_into(roots, LIMITS)
}

/// `scan` with these limits: the tests make them small enough to reach.
fn walk_into(roots: &[PathBuf], limits: Limits) -> Scan {
    let mut collector = Collector::new(limits);
    let mut node_modules = Vec::new();
    let mut full = false;
    for root in roots {
        // Reached: the folders after this one have nothing left to add.
        if full {
            break;
        }
        let mut found: Vec<PathBuf> = Vec::new();
        for entry in super::project_walk(root).flatten() {
            let path = entry.path();
            let Some(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                let candidate = path.join("node_modules");
                if candidate.is_dir() {
                    found.push(candidate);
                }
                continue;
            }
            if !file_type.is_file() || !is_wanted(path) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if collector.add(path, meta.len()) == Walk::Stop {
                full = true;
                break;
            }
        }
        found.sort();
        node_modules.extend(found);
    }
    let (files, truncated) = collector.finish();
    Scan {
        files: LoadedFiles {
            files,
            links: Vec::new(),
            truncated,
        },
        node_modules,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typings::test_support::{relative, relative_paths, temp_project};

    #[test]
    fn loads_sources_and_skips_dependencies_and_ignored_files() {
        let dir = temp_project(
            "sources-test",
            &[
                ("src/a.ts", "export class A {}"),
                ("src/b.jsx", "export const B = 1"),
                ("src/readme.md", "#"),
                ("package.json", "{}"),
                ("tsconfig.json", "{}"),
                ("@scope/lib/tsconfig.build.json", "{}"),
                ("tsconfig.json.bak", "{}"),
                ("node_modules/x/index.js", "x"),
                ("packages/app/node_modules/y/index.js", "y"),
                ("dist/bundle.js", "x"),
                ("generated/out.ts", "x"),
                (".gitignore", "generated/\n"),
            ],
        );
        let scan = scan(std::slice::from_ref(&dir));
        assert!(!scan.files.truncated);
        assert_eq!(
            relative_paths(&dir, &scan.files.files),
            [
                "@scope/lib/tsconfig.build.json",
                "package.json",
                "src/a.ts",
                "src/b.jsx",
                "tsconfig.json"
            ]
        );
        let folders: Vec<String> = scan
            .node_modules
            .iter()
            .map(|p| relative(&dir, &p.to_string_lossy()))
            .collect();
        assert_eq!(folders, ["node_modules", "packages/app/node_modules"]);
    }
}

#[cfg(test)]
mod folder_tests {
    use super::*;
    use crate::typings::test_support::temp_project;

    fn inside(dir: &Path, scan: &Scan) -> Vec<String> {
        let mut out: Vec<String> = scan
            .files
            .files
            .iter()
            .filter_map(|file| {
                Path::new(&file.path)
                    .strip_prefix(dir)
                    .ok()
                    .map(|rest| rest.to_string_lossy().replace('\\', "/"))
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn two_roots_sources_are_both_loaded() {
        let api = temp_project(
            "two-roots-api",
            &[("src/a.ts", "export const a = 1"), ("package.json", "{}")],
        );
        let web = temp_project(
            "two-roots-web",
            &[("src/b.ts", "export const b = 1"), ("tsconfig.json", "{}")],
        );
        let scan = scan(&[api.clone(), web.clone()]);
        assert!(!scan.files.truncated);
        assert_eq!(inside(&api, &scan), ["package.json", "src/a.ts"]);
        assert_eq!(inside(&web, &scan), ["src/b.ts", "tsconfig.json"]);
        let _ = std::fs::remove_dir_all(&api);
        let _ = std::fs::remove_dir_all(&web);
    }

    #[test]
    fn two_roots_share_one_limit() {
        let api = temp_project("shared-limit-api", &[("a.ts", "1"), ("b.ts", "2")]);
        let web = temp_project("shared-limit-web", &[("c.ts", "1"), ("d.ts", "2")]);
        let scan = walk_into(
            &[api.clone(), web.clone()],
            Limits {
                max_files: 3,
                max_file_bytes: 512 * 1024,
                max_total_bytes: 40 * 1024 * 1024,
            },
        );
        assert_eq!(
            scan.files.files.len(),
            3,
            "the second folder has what the first left"
        );
        assert!(
            scan.files.truncated,
            "the limit is the project's, not each folder's"
        );
        let _ = std::fs::remove_dir_all(&api);
        let _ = std::fs::remove_dir_all(&web);
    }
}

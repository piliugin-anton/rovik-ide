//! What the editor's TypeScript IntelliSense gets from a project: its own
//! JavaScript/TypeScript sources (`sources`) and the type declarations of
//! the dependencies those sources reach (`closure`). Monaco's TypeScript
//! worker only knows files it's given, so without these an import of
//! anything not open in a tab can't be resolved: no go-to-definition,
//! hover types or completions for it.
//!
//! The dependencies are loaded on demand, as tsc reads them: from the
//! packages the sources import (and the `@types` packages every config
//! sees), through the imports of their own declarations, to the packages
//! those reach, and no further (`closure`). A `node_modules` of a few
//! thousand packages holds the declarations of build tools, plugins and
//! transitive dependencies nothing in the project can name; loading them
//! all was tens of thousands of files and a hundred megabytes crossing to
//! the webview on every project open, seconds of waiting, and a limit that
//! then cut off packages the code did import. The closure is what the
//! code can reach, usually a few hundred files, and the limits
//! (`budget::Limits`) are a safety net against a runaway, not something
//! a large project meets.
//!
//! An import typed later, of an installed package nothing imported before,
//! is loaded the same way, from that one import (`load_dependencies`).
//!
//! No Tauri here: the app's commands (src-tauri/src/typings.rs) and a
//! daemon can both load a project's definitions.

#[cfg(test)]
mod bench;
pub mod budget;
mod closure;
mod imports;
mod resolve;
pub mod sources;

pub use closure::{LoadedPackage, LoadedTypings};
pub use imports::ImportSite;

use closure::Closure;
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Folders never worth looking into, even when not gitignored.
pub const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "dist",
    "build",
    "out",
    "target",
    ".next",
    ".svelte-kit",
    "coverage",
];

/// A walk of the project as git sees it (.gitignore respected, hidden
/// files skipped), with dependencies and build output pruned.
pub(crate) fn project_walk(root: &Path) -> ignore::Walk {
    WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .require_git(false)
        .filter_entry(|e| {
            !(e.file_type().is_some_and(|t| t.is_dir())
                && e.file_name()
                    .to_str()
                    .is_some_and(|n| SKIP_DIRS.contains(&n)))
        })
        .build()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LoadedFile {
    /// Absolute path, as the file tree uses it.
    pub path: String,
    pub content: String,
}

/// A symbolic link to a folder, as `node_modules` is full of them (pnpm,
/// workspaces, `npm link`). Files are loaded under their real path only;
/// the editor resolves a path through the links, as Node does.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Link {
    pub path: String,
    pub target: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LoadedFiles {
    pub files: Vec<LoadedFile>,
    pub links: Vec<Link>,
    /// A limit was hit: some files weren't loaded.
    pub truncated: bool,
}

/// Everything the editor loads when a project opens, from one walk of it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDefinitions {
    pub sources: LoadedFiles,
    pub typings: LoadedTypings,
}

/// The project's sources and the dependencies they reach. Its folders are
/// a project's one, or a workspace's several (C06).
pub fn load_definitions(roots: &[PathBuf]) -> ProjectDefinitions {
    let scan = sources::scan(roots);
    let typings = closure_of(&scan).finish();
    ProjectDefinitions {
        sources: scan.files,
        typings,
    }
}

/// The dependencies the project's sources reach. The sources are read for
/// their imports and not returned: for a refresh after an install.
pub fn load_typings(roots: &[PathBuf]) -> LoadedTypings {
    let scan = sources::scan(roots);
    closure_of(&scan).finish()
}

/// The project's sources alone (within limits).
pub fn load_sources(roots: &[PathBuf]) -> LoadedFiles {
    sources::scan(roots).files
}

/// The packages these imports reach, less those under `exclude` (package
/// folders already loaded, whose closure the editor has): for an import
/// typed into a file, of a package nothing imported before. Resolution
/// starts from each site's folder, so no project root is needed.
pub fn load_dependencies(sites: &[ImportSite], exclude: &[String]) -> LoadedTypings {
    let mut closure = Closure::new(exclude.iter().map(PathBuf::from));
    for site in sites {
        let from = Path::new(&site.from);
        let dir = from.parent().unwrap_or(from);
        for specifier in &site.specifiers {
            closure.want(dir, specifier);
        }
    }
    closure.run();
    closure.finish()
}

/// The closure of a scanned project: seeded with every source's imports,
/// every config's `types` and JSX source, every package the manifests
/// declare as a runtime dependency, and every `@types` package in the
/// project's `node_modules` folders (TypeScript takes those in as globals,
/// imported or not).
fn closure_of(scan: &sources::Scan) -> Closure {
    let mut closure = Closure::new(std::iter::empty());
    for file in &scan.files.files {
        let path = Path::new(&file.path);
        let dir = path.parent().unwrap_or(path);
        let seeds = if sources::is_config(path) {
            imports::config_seeds(&file.content)
        } else if sources::is_manifest(path) {
            imports::manifest_seeds(&file.content)
        } else {
            imports::specifiers(&file.content)
        };
        for specifier in seeds {
            closure.want(dir, &specifier);
        }
    }
    for dir in &scan.node_modules {
        closure.want_types_in(dir);
    }
    closure.run();
    closure
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A fresh temporary folder with these files (relative path, content).
    pub fn temp_project(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rovik-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (path, content) in files {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        dir
    }

    /// Loaded paths relative to `dir`, forward slashes, sorted.
    pub fn relative_paths(dir: &Path, files: &[super::LoadedFile]) -> Vec<String> {
        let mut out: Vec<String> = files.iter().map(|f| relative(dir, &f.path)).collect();
        out.sort();
        out
    }

    pub fn relative(dir: &Path, path: &str) -> String {
        Path::new(path)
            .strip_prefix(dir)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/")
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{relative, relative_paths, temp_project};
    use super::*;

    #[test]
    fn a_project_loads_its_sources_and_the_dependencies_they_reach() {
        let dir = temp_project(
            "definitions",
            &[
                (".gitignore", "node_modules\n"),
                ("package.json", r#"{"dependencies":{"declared":"1"}}"#),
                (
                    "tsconfig.json",
                    "{\"compilerOptions\":{\"types\":[\"vite/client\"]}}",
                ),
                ("src/app.ts", "import { a } from 'a';\nimport 'node:fs';"),
                ("node_modules/a/package.json", "{\"types\":\"index.d.ts\"}"),
                (
                    "node_modules/a/index.d.ts",
                    "import type { B } from 'b'; export declare const a: B;",
                ),
                ("node_modules/a/index.js", "x"),
                ("node_modules/a/test/a.d.ts", "export {}"),
                ("node_modules/b/index.d.ts", "export interface B {}"),
                ("node_modules/declared/index.d.ts", "export {}"),
                ("node_modules/vite/client.d.ts", "interface ImportMeta {}"),
                ("node_modules/vite/package.json", "{}"),
                (
                    "node_modules/@types/node/index.d.ts",
                    "declare var process: any",
                ),
                ("node_modules/unused/index.d.ts", "export {}"),
                ("node_modules/eslint/lib/index.d.ts", "export {}"),
            ],
        );
        let loaded = load_definitions(std::slice::from_ref(&dir));
        assert!(!loaded.sources.truncated && !loaded.typings.truncated);
        assert_eq!(
            relative_paths(&dir, &loaded.sources.files),
            ["package.json", "src/app.ts", "tsconfig.json"]
        );
        assert_eq!(
            relative_paths(&dir, &loaded.typings.files),
            [
                "node_modules/@types/node/index.d.ts",
                "node_modules/a/index.d.ts",
                "node_modules/a/package.json",
                "node_modules/b/index.d.ts",
                "node_modules/declared/index.d.ts",
                "node_modules/vite/client.d.ts",
                "node_modules/vite/package.json",
            ]
        );
        let mut names: Vec<&str> = loaded
            .typings
            .packages
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        names.sort();
        assert_eq!(names, ["@types/node", "a", "b", "declared", "vite"]);
        assert_eq!(
            relative(
                &dir,
                &loaded
                    .typings
                    .packages
                    .iter()
                    .find(|p| p.name == "b")
                    .unwrap()
                    .dir
            ),
            "node_modules/b"
        );
        assert!(loaded.typings.missing.is_empty());
    }

    #[test]
    fn a_later_import_loads_its_package_and_what_that_reaches() {
        let dir = temp_project(
            "later-import",
            &[
                ("src/app.ts", ""),
                ("node_modules/a/index.d.ts", "export * from 'b';"),
                ("node_modules/b/index.d.ts", "export {}"),
                ("node_modules/c/index.d.ts", "export {}"),
            ],
        );
        let site = ImportSite {
            from: dir.join("src/app.ts").to_string_lossy().into_owned(),
            specifiers: vec!["a".into(), "./local".into(), "nowhere".into(), "c".into()],
        };
        let already = dir.join("node_modules/c").to_string_lossy().into_owned();
        let loaded = load_dependencies(&[site], &[already]);
        assert_eq!(
            relative_paths(&dir, &loaded.files),
            ["node_modules/a/index.d.ts", "node_modules/b/index.d.ts"]
        );
        assert_eq!(loaded.missing, ["nowhere"]);
    }
}

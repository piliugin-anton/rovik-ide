//! The project's own JavaScript/TypeScript sources. Walked the way git sees
//! the project (.gitignore respected), skipping dependencies and build
//! output. These are the files of the TypeScript worker's program, so an
//! import of a file no tab has open still resolves.

use super::budget::{Collector, Limits, Walk};
use super::LoadedFiles;
use ignore::WalkBuilder;
use std::path::Path;

const EXTENSIONS: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];
/// Folders never worth loading even when not gitignored.
const SKIP_DIRS: &[&str] = &[
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
const LIMITS: Limits = Limits {
    max_files: 5_000,
    max_file_bytes: 512 * 1024,
    max_total_bytes: 40 * 1024 * 1024,
};

fn is_source_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e))
}

pub fn collect(root: &Path) -> LoadedFiles {
    let mut collector = Collector::new(LIMITS);
    let walker = WalkBuilder::new(root)
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
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        if !entry.file_type().is_some_and(|t| t.is_file()) || !is_source_file(path) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if collector.add(path, meta.len()) == Walk::Stop {
            break;
        }
    }
    collector.finish()
}

#[tauri::command]
pub async fn load_project_sources(root: String) -> Result<LoadedFiles, String> {
    super::load_from(root, collect).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typings::test_support::{relative_paths, temp_project};

    #[test]
    fn loads_sources_and_skips_dependencies_and_ignored_files() {
        let dir = temp_project(
            "sources-test",
            &[
                ("src/a.ts", "export class A {}"),
                ("src/b.jsx", "export const B = 1"),
                ("src/readme.md", "#"),
                ("node_modules/x/index.js", "x"),
                ("dist/bundle.js", "x"),
                ("generated/out.ts", "x"),
                (".gitignore", "generated/\n"),
            ],
        );
        let loaded = collect(&dir);
        assert!(!loaded.truncated);
        assert_eq!(relative_paths(&dir, &loaded), ["src/a.ts", "src/b.jsx"]);
    }
}

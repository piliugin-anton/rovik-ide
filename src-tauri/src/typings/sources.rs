//! The project's own JavaScript/TypeScript sources, with its tsconfig /
//! jsconfig files (which config owns which file, workspace members'
//! included: src/lib/tsconfig.ts) and package.json files (a file's module
//! format and a package's `exports`, under node16 resolution). Walked the
//! way git sees the project (.gitignore respected), skipping dependencies
//! and build output. The sources are the files of the TypeScript worker's
//! program, so an import of a file no tab has open still resolves.

use super::budget::{Collector, Limits, Walk};
use super::LoadedFiles;
use std::path::Path;

const EXTENSIONS: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];
const LIMITS: Limits = Limits {
    max_files: 5_000,
    max_file_bytes: 512 * 1024,
    max_total_bytes: 40 * 1024 * 1024,
};

fn is_wanted(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let is_config =
        (name.starts_with("tsconfig") || name.starts_with("jsconfig")) && name.ends_with(".json");
    is_config
        || name == "package.json"
        || path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| EXTENSIONS.contains(&e))
}

pub fn collect(root: &Path) -> LoadedFiles {
    let mut collector = Collector::new(LIMITS);
    for entry in super::project_walk(root).flatten() {
        let path = entry.path();
        if !entry.file_type().is_some_and(|t| t.is_file()) || !is_wanted(path) {
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
                ("package.json", "{}"),
                ("tsconfig.json", "{}"),
                ("@scope/lib/tsconfig.build.json", "{}"),
                ("tsconfig.json.bak", "{}"),
                ("node_modules/x/index.js", "x"),
                ("dist/bundle.js", "x"),
                ("generated/out.ts", "x"),
                (".gitignore", "generated/\n"),
            ],
        );
        let loaded = collect(&dir);
        assert!(!loaded.truncated);
        assert_eq!(
            relative_paths(&dir, &loaded),
            [
                "@scope/lib/tsconfig.build.json",
                "package.json",
                "src/a.ts",
                "src/b.jsx",
                "tsconfig.json"
            ]
        );
    }
}
